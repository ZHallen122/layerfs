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
    FileIdType, GetFinalPathNameByHandleW, OpenFileById, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0,
};
use windows_sys::Win32::System::Ioctl::{
    FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, READ_USN_JOURNAL_DATA_V1,
    USN_JOURNAL_DATA_V0, USN_RECORD_V2,
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
const ACCESS_DENIED: i32 = 5;
const INVALID_PARAMETER: i32 = 87;
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

/// A handle on the volume a directory is on, for its journal and its files.
pub struct Volume(std::fs::File);

const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_LIST_DIRECTORY: u32 = 0x1;
const FILE_TRAVERSE: u32 = 0x20;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
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

    /// The path of the file with this identity now, spelled as the volume
    /// holds it (`\\?\C:\...`); `None` when no such file can be opened. A
    /// reparse point is opened itself, never followed.
    pub fn path(&self, id: u64) -> std::io::Result<Option<PathBuf>> {
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
                Some(FILE_NOT_FOUND | ACCESS_DENIED | INVALID_PARAMETER) => Ok(None),
                _ => Err(error),
            };
        }
        // SAFETY: the handle was just opened and is owned from here.
        let file = unsafe { std::fs::File::from_raw_handle(handle.cast()) };
        let mut wide = vec![0u16; 512];
        loop {
            // SAFETY: the handle is open and `wide` is writable for its length.
            let length = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle() as HANDLE,
                    wide.as_mut_ptr(),
                    wide.len() as u32,
                    0,
                )
            } as usize;
            if length == 0 {
                // Deleted between the open and the question.
                return Ok(None);
            }
            if length < wide.len() {
                wide.truncate(length);
                return Ok(Some(PathBuf::from(std::ffi::OsString::from_wide(&wide))));
            }
            wide.resize(length + 1, 0);
        }
    }
}
