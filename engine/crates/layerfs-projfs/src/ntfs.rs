//! The crate's second audited FFI module: what the change tracker reads from
//! NTFS itself, below the ProjFS filter.
//!
//! The USN change journal of a volume lists every change to every file on
//! it, in order. An administrator can read it with file names. Without
//! administrator rights `FSCTL_READ_UNPRIVILEGED_USN_JOURNAL` returns the
//! same records with the names left out: each still carries the file's
//! identity, its parent directory's identity, the reasons and the
//! attributes NTFS holds, which the ProjFS filter hides from every process.
//! A path is recovered by opening the file by its identity. The read is made
//! through a handle to a directory on the volume and must start at a record
//! boundary; both were found by trying, the call being barely documented.
#![allow(unsafe_code)]

use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FileIdType, FindClose, FindFirstFileNameW, FindNextFileNameW, GetFinalPathNameByHandleW,
    OpenFileById, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_DESCRIPTOR,
    FILE_ID_DESCRIPTOR_0,
};
use windows_sys::Win32::System::Ioctl::{
    FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_FILE_USN_DATA, FSCTL_READ_UNPRIVILEGED_USN_JOURNAL,
    READ_USN_JOURNAL_DATA_V1, USN_JOURNAL_DATA_V0, USN_RECORD_V2,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

/// Why a file changed, as a bit set: the journal's reason codes.
pub mod reason {
    pub const DATA_OVERWRITE: u32 = 0x1;
    pub const DATA_EXTEND: u32 = 0x2;
    pub const DATA_TRUNCATION: u32 = 0x4;
    pub const NAMED_DATA: u32 = 0x10 | 0x20 | 0x40;
    pub const FILE_CREATE: u32 = 0x100;
    pub const FILE_DELETE: u32 = 0x200;
    pub const EA_CHANGE: u32 = 0x400;
    pub const RENAME_OLD_NAME: u32 = 0x1000;
    pub const RENAME_NEW_NAME: u32 = 0x2000;
    pub const BASIC_INFO_CHANGE: u32 = 0x8000;
    pub const HARD_LINK_CHANGE: u32 = 0x1_0000;
    pub const REPARSE_POINT_CHANGE: u32 = 0x10_0000;
    pub const STREAM_CHANGE: u32 = 0x20_0000;
    pub const CLOSE: u32 = 0x8000_0000;
    /// The file's unnamed data changed.
    pub const DATA: u32 = DATA_OVERWRITE | DATA_EXTEND | DATA_TRUNCATION;
    /// A name appeared in or left a directory.
    pub const NAME: u32 =
        FILE_CREATE | FILE_DELETE | RENAME_OLD_NAME | RENAME_NEW_NAME | HARD_LINK_CHANGE;
}

/// Attribute bits as NTFS holds them.
pub mod attribute {
    pub const READ_ONLY: u32 = 0x1;
    pub const HIDDEN: u32 = 0x2;
    pub const SYSTEM: u32 = 0x4;
    pub const DIRECTORY: u32 = 0x10;
    pub const REPARSE_POINT: u32 = 0x400;
}

/// One change journal record, without the name an unprivileged read omits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Record {
    /// The file's identity on its volume.
    pub id: u64,
    /// The identity of the directory holding the name the record is about.
    pub parent: u64,
    pub reason: u32,
    /// The attributes NTFS held after the change, reparse point included.
    pub attributes: u32,
}

/// A volume's journal at one moment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Journal {
    /// Changes when the journal is deleted and created again.
    pub id: u64,
    /// The oldest record that can still be read.
    pub first: i64,
    /// Where the next record will be written.
    pub next: i64,
}

/// Why the journal could not be read.
#[derive(Debug)]
pub enum JournalError {
    /// The volume has no journal, the journal was replaced, or the records
    /// asked for are gone. Everything since the position is unknown.
    Lost(String),
    Io(std::io::Error),
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lost(why) => write!(f, "the change journal cannot answer: {why}"),
            Self::Io(error) => write!(f, "change journal: {error}"),
        }
    }
}

const INVALID_FUNCTION: i32 = 1;
const FILE_NOT_FOUND: i32 = 2;
const HANDLE_EOF: i32 = 38;
const INVALID_PARAMETER: i32 = 87;
const MORE_DATA: i32 = 234;
const JOURNAL_DELETE_IN_PROGRESS: i32 = 1178;
const JOURNAL_NOT_ACTIVE: i32 = 1179;
const JOURNAL_ENTRY_DELETED: i32 = 1181;

fn journal_error(error: std::io::Error) -> JournalError {
    match error.raw_os_error() {
        Some(
            INVALID_FUNCTION
            | INVALID_PARAMETER
            | JOURNAL_DELETE_IN_PROGRESS
            | JOURNAL_NOT_ACTIVE
            | JOURNAL_ENTRY_DELETED,
        ) => JournalError::Lost(error.to_string()),
        _ => JournalError::Io(error),
    }
}

/// The number of the newest change journal record of `file`. Every
/// journaled change to the file gives it a new, larger number, so an equal
/// number means the file has not changed since, whatever its size and time
/// say: a write through a mapped view moves neither.
pub fn last_change(file: &std::fs::File) -> std::io::Result<i64> {
    // A record with the longest name NTFS allows, 8-byte aligned.
    let mut output = [0u64; 96];
    let mut returned = 0u32;
    // SAFETY: the handle is open for the borrow's duration and `output` is
    // writable for the size passed.
    let done = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            FSCTL_READ_FILE_USN_DATA,
            std::ptr::null(),
            0,
            output.as_mut_ptr().cast(),
            std::mem::size_of_val(&output) as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if done == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the call wrote a record of at least the fixed part.
    let record = unsafe { &*output.as_ptr().cast::<USN_RECORD_V2>() };
    if record.MajorVersion != 2
        || (returned as usize) < std::mem::offset_of!(USN_RECORD_V2, FileName)
    {
        return Err(std::io::Error::other("a record of an unexpected shape"));
    }
    Ok(record.Usn)
}

/// A handle on the volume a directory is on, for its journal and its files.
pub struct Volume(std::fs::File);

const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_LIST_DIRECTORY: u32 = 0x1;
const FILE_TRAVERSE: u32 = 0x20;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
/// `GetFinalPathNameByHandleW`: the path without its volume.
const VOLUME_NAME_NONE: u32 = 0x4;
const SHARE_ALL: u32 = 7;

impl Volume {
    /// Opens the volume through `directory`, which stays open with it.
    pub fn open(directory: &Path) -> std::io::Result<Self> {
        std::fs::OpenOptions::new()
            .access_mode(SYNCHRONIZE | FILE_LIST_DIRECTORY | FILE_TRAVERSE)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(directory)
            .map(Self)
    }

    fn handle(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }

    /// The identity of the directory the volume was opened through.
    pub fn directory(&self) -> std::io::Result<crate::sys::Identity> {
        crate::sys::identity(&self.0)
    }

    /// The journal's identity and bounds now.
    pub fn journal(&self) -> Result<Journal, JournalError> {
        let (mut data, mut returned) = (USN_JOURNAL_DATA_V0::default(), 0u32);
        // SAFETY: the handle is open; `data` is writable and its size passed.
        let done = unsafe {
            DeviceIoControl(
                self.handle(),
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                std::ptr::from_mut(&mut data).cast(),
                std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if done == 0 {
            return Err(journal_error(std::io::Error::last_os_error()));
        }
        Ok(Journal {
            id: data.UsnJournalID,
            first: data.FirstUsn,
            next: data.NextUsn,
        })
    }

    /// Hands every record from `from` to the journal's end to `each`, in
    /// order, and returns where the next read starts. `from` is a position
    /// this function or [`Self::journal`] returned.
    pub fn read(
        &self,
        journal: u64,
        from: i64,
        each: &mut dyn FnMut(&Record),
    ) -> Result<i64, JournalError> {
        // 64 KiB, aligned for the records' 64-bit fields.
        let mut output = vec![0u64; 8192];
        let mut at = from;
        loop {
            let input = READ_USN_JOURNAL_DATA_V1 {
                StartUsn: at,
                ReasonMask: u32::MAX,
                ReturnOnlyOnClose: 0,
                Timeout: 0,
                BytesToWaitFor: 0,
                UsnJournalID: journal,
                MinMajorVersion: 2,
                MaxMajorVersion: 2,
            };
            let mut returned = 0u32;
            // SAFETY: the handle is open; `input` outlives the call; `output`
            // is writable for the size passed.
            let done = unsafe {
                DeviceIoControl(
                    self.handle(),
                    FSCTL_READ_UNPRIVILEGED_USN_JOURNAL,
                    std::ptr::from_ref(&input).cast(),
                    std::mem::size_of::<READ_USN_JOURNAL_DATA_V1>() as u32,
                    output.as_mut_ptr().cast(),
                    (output.len() * 8) as u32,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if done == 0 {
                return Err(journal_error(std::io::Error::last_os_error()));
            }
            let returned = returned as usize;
            if returned < 8 {
                return Ok(at);
            }
            let next = output[0] as i64;
            let header = std::mem::offset_of!(USN_RECORD_V2, FileName);
            let mut offset = 8;
            while offset + header <= returned {
                // SAFETY: the record lies inside the bytes the call wrote,
                // at an offset the previous record's length gave; records
                // are 8-byte aligned in an 8-byte aligned buffer.
                let record = unsafe {
                    &*output
                        .as_ptr()
                        .cast::<u8>()
                        .add(offset)
                        .cast::<USN_RECORD_V2>()
                };
                let length = record.RecordLength as usize;
                if record.MajorVersion != 2 || length < header || offset + length > returned {
                    return Err(JournalError::Lost(
                        "a record of an unexpected shape".to_owned(),
                    ));
                }
                each(&Record {
                    id: record.FileReferenceNumber,
                    parent: record.ParentFileReferenceNumber,
                    reason: record.Reason,
                    attributes: record.FileAttributes,
                });
                offset += length;
            }
            if next == at || returned == 8 {
                return Ok(next);
            }
            at = next;
        }
    }

    /// Opens the file with this identity; `None` when the volume has no such
    /// file. A reparse point is opened itself, never followed. A file that
    /// exists and may not be opened is an error, not an absent file: the
    /// caller cannot tell what became of it.
    fn open_by_identity(&self, id: u64) -> std::io::Result<Option<std::fs::File>> {
        let descriptor = FILE_ID_DESCRIPTOR {
            dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: id as i64 },
        };
        // SAFETY: the hint handle is open and `descriptor` outlives the call.
        let handle = unsafe {
            OpenFileById(
                self.handle(),
                &descriptor,
                FILE_READ_ATTRIBUTES,
                SHARE_ALL,
                std::ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(FILE_NOT_FOUND | INVALID_PARAMETER) => Ok(None),
                _ => Err(error),
            };
        }
        // SAFETY: the handle was just opened and is owned from here.
        Ok(Some(unsafe {
            std::fs::File::from_raw_handle(handle.cast())
        }))
    }

    /// The path `file` was opened at, as the volume spells it; `None` when
    /// the file was deleted since. `flags` chooses the form of the volume's
    /// name: 0 for `\\?\C:\...`, [`VOLUME_NAME_NONE`] for none.
    fn spelled(file: &std::fs::File, flags: u32) -> Option<Vec<u16>> {
        let mut wide = vec![0u16; 512];
        loop {
            // SAFETY: the handle is open and `wide` is writable for its length.
            let length = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle() as HANDLE,
                    wide.as_mut_ptr(),
                    wide.len() as u32,
                    flags,
                )
            } as usize;
            if length == 0 {
                return None;
            }
            if length < wide.len() {
                wide.truncate(length);
                return Some(wide);
            }
            wide.resize(length + 1, 0);
        }
    }

    /// The path of the file with this identity now, spelled as the volume
    /// holds it (`\\?\C:\...`); `None` when the volume has no such file.
    pub fn path(&self, id: u64) -> std::io::Result<Option<PathBuf>> {
        let Some(file) = self.open_by_identity(id)? else {
            return Ok(None);
        };
        Ok(Self::spelled(&file, 0).map(|wide| PathBuf::from(std::ffi::OsString::from_wide(&wide))))
    }

    /// Every path of the file with this identity now: one, or one for each
    /// of its hard links. Empty when the volume has no such file.
    pub fn paths(&self, id: u64) -> std::io::Result<Vec<PathBuf>> {
        let Some(file) = self.open_by_identity(id)? else {
            return Ok(Vec::new());
        };
        let Some(full) = Self::spelled(&file, 0) else {
            return Ok(Vec::new());
        };
        let own = PathBuf::from(std::ffi::OsString::from_wide(&full));
        let identity = crate::sys::identity(&file)?;
        if identity.links < 2 || identity.attributes & attribute::DIRECTORY != 0 {
            return Ok(vec![own]);
        }
        // The other names are told without the volume: what precedes this
        // name's own volume-less form is the volume.
        let Some(bare) = Self::spelled(&file, VOLUME_NAME_NONE) else {
            return Ok(Vec::new());
        };
        let volume = &full[..full.len().saturating_sub(bare.len())];
        let mut name = full.clone();
        name.push(0);
        let mut found = Vec::new();
        let mut link = vec![0u16; 512];
        let mut length = link.len() as u32;
        // SAFETY: `name` ends in NUL; `link` is writable for `length` units.
        let mut search =
            unsafe { FindFirstFileNameW(name.as_ptr(), 0, &mut length, link.as_mut_ptr()) };
        while search == INVALID_HANDLE_VALUE {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(MORE_DATA) {
                // Renamed or removed since it was opened: its own name stands.
                return Ok(vec![own]);
            }
            link.resize(length as usize + 1, 0);
            length = link.len() as u32;
            // SAFETY: as above, with the larger buffer.
            search =
                unsafe { FindFirstFileNameW(name.as_ptr(), 0, &mut length, link.as_mut_ptr()) };
        }
        loop {
            let units = (length as usize).min(link.len());
            let text = &link[..units];
            let text = &text[..text.iter().position(|unit| *unit == 0).unwrap_or(units)];
            let mut whole = volume.to_vec();
            whole.extend_from_slice(text);
            found.push(PathBuf::from(std::ffi::OsString::from_wide(&whole)));
            length = link.len() as u32;
            // SAFETY: `search` is open; `link` is writable for `length` units.
            if unsafe { FindNextFileNameW(search, &mut length, link.as_mut_ptr()) } != 0 {
                continue;
            }
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(MORE_DATA) => {
                    link.resize(length as usize + 1, 0);
                    length = link.len() as u32;
                    // SAFETY: as above; the same name is asked for again.
                    if unsafe { FindNextFileNameW(search, &mut length, link.as_mut_ptr()) } != 0 {
                        continue;
                    }
                    break;
                }
                Some(HANDLE_EOF) => break,
                _ => {
                    // SAFETY: `search` is open and closed once.
                    unsafe { FindClose(search) };
                    return Err(error);
                }
            }
        }
        // SAFETY: `search` is open and closed once.
        unsafe { FindClose(search) };
        Ok(found)
    }
}
