//! The crate's one audited FFI module: the Projected File System library,
//! loaded at run time, behind safe types.
//!
//! `ProjectedFSLib.dll` exists only where the optional Windows feature
//! `Client-ProjFS` is enabled, so it is loaded with `LoadLibraryExW` and
//! answered `Unavailable` when absent instead of failing the process's load.
//! Every pointer the library hands a callback is read here, inside the
//! callback's own duration, and never kept. A panic in a handler is caught
//! and answered as a failed request; it never unwinds into the library.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use windows_sys::core::{GUID, HRESULT, PCWSTR};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
};
use windows_sys::Win32::Storage::ProjectedFileSystem as prj;
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

/// A Windows result code as the library returns it.
pub type Hresult = i32;

/// The `HRESULT` of a Win32 error code.
pub const fn win32(code: u32) -> Hresult {
    if code == 0 {
        0
    } else {
        ((code & 0xFFFF) | 0x8007_0000) as i32
    }
}
pub const NOT_FOUND: Hresult = win32(2);
pub const PATH_NOT_FOUND: Hresult = win32(3);
pub const INVALID_NAME: Hresult = win32(123);
pub const INSUFFICIENT_BUFFER: Hresult = win32(122);
pub const INVALID_PARAMETER: Hresult = win32(87);
pub const IO_ERROR: Hresult = win32(1117);
const E_FAIL: Hresult = 0x8000_4005_u32 as i32;

/// On-disk states of an item below a virtualization root, as a bit set.
pub mod state {
    pub const PLACEHOLDER: i32 = 1;
    pub const HYDRATED: i32 = 2;
    pub const DIRTY: i32 = 4;
    pub const FULL: i32 = 8;
    pub const TOMBSTONE: i32 = 16;
}

/// Notifications the provider asks for: every change to a name, to file
/// data or to links. Opens and unmodified closes are not requested.
pub const NOTIFICATIONS: u32 = prj::PRJ_NOTIFY_NEW_FILE_CREATED
    | prj::PRJ_NOTIFY_FILE_OVERWRITTEN
    | prj::PRJ_NOTIFY_PRE_DELETE
    | prj::PRJ_NOTIFY_PRE_RENAME
    | prj::PRJ_NOTIFY_PRE_SET_HARDLINK
    | prj::PRJ_NOTIFY_FILE_RENAMED
    | prj::PRJ_NOTIFY_HARDLINK_CREATED
    | prj::PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_MODIFIED
    | prj::PRJ_NOTIFY_FILE_HANDLE_CLOSED_FILE_DELETED
    | prj::PRJ_NOTIFY_FILE_PRE_CONVERT_TO_FULL;

/// Flags for [`Instance::delete_item`] and [`Instance::update_item`].
pub const ALLOW_DIRTY_METADATA: i32 = prj::PRJ_UPDATE_ALLOW_DIRTY_METADATA;
pub const ALLOW_DIRTY_DATA: i32 = prj::PRJ_UPDATE_ALLOW_DIRTY_DATA;
pub const ALLOW_TOMBSTONE: i32 = prj::PRJ_UPDATE_ALLOW_TOMBSTONE;
pub const ALLOW_READ_ONLY: i32 = prj::PRJ_UPDATE_ALLOW_READ_ONLY;

type Context = prj::PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT;
type Mark = unsafe extern "system" fn(
    PCWSTR,
    PCWSTR,
    *const prj::PRJ_PLACEHOLDER_VERSION_INFO,
    *const GUID,
) -> HRESULT;
type Start = unsafe extern "system" fn(
    PCWSTR,
    *const prj::PRJ_CALLBACKS,
    *const c_void,
    *const prj::PRJ_STARTVIRTUALIZING_OPTIONS,
    *mut Context,
) -> HRESULT;
type Stop = unsafe extern "system" fn(Context);
type FillEntry = unsafe extern "system" fn(
    PCWSTR,
    *const prj::PRJ_FILE_BASIC_INFO,
    prj::PRJ_DIR_ENTRY_BUFFER_HANDLE,
) -> HRESULT;
type WritePlaceholder =
    unsafe extern "system" fn(Context, PCWSTR, *const prj::PRJ_PLACEHOLDER_INFO, u32) -> HRESULT;
type WriteData =
    unsafe extern "system" fn(Context, *const GUID, *const c_void, u64, u32) -> HRESULT;
type Allocate = unsafe extern "system" fn(Context, usize) -> *mut c_void;
type Free = unsafe extern "system" fn(*const c_void);
type DiskState = unsafe extern "system" fn(PCWSTR, *mut i32) -> HRESULT;
type DeleteItem = unsafe extern "system" fn(Context, PCWSTR, i32, *mut i32) -> HRESULT;
type UpdateItem = unsafe extern "system" fn(
    Context,
    PCWSTR,
    *const prj::PRJ_PLACEHOLDER_INFO,
    u32,
    i32,
    *mut i32,
) -> HRESULT;
type Match = unsafe extern "system" fn(PCWSTR, PCWSTR) -> u8;
type Compare = unsafe extern "system" fn(PCWSTR, PCWSTR) -> i32;
type Wild = unsafe extern "system" fn(PCWSTR) -> u8;

struct Api {
    mark: Mark,
    start: Start,
    stop: Stop,
    fill: FillEntry,
    placeholder: WritePlaceholder,
    data: WriteData,
    allocate: Allocate,
    free: Free,
    disk_state: DiskState,
    delete: DeleteItem,
    update: UpdateItem,
    matches: Match,
    compare: Compare,
    wild: Wild,
}

/// The Projected File System library is not on this machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

static API: OnceLock<Result<Api, Unavailable>> = OnceLock::new();

fn api() -> Result<&'static Api, Unavailable> {
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Api, Unavailable> {
    let name = wide("ProjectedFSLib.dll");
    // SAFETY: `name` is a NUL-terminated wide string that outlives the call.
    let module = unsafe {
        LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err(Unavailable(format!(
            "ProjectedFSLib.dll is not installed ({}). Enable the optional Windows feature \
             once, in an elevated PowerShell: Enable-WindowsOptionalFeature -Online \
             -FeatureName Client-ProjFS -NoRestart",
            std::io::Error::last_os_error()
        )));
    }
    macro_rules! symbol {
        ($name:literal, $kind:ty) => {{
            // SAFETY: `module` is a loaded library and the name is a
            // NUL-terminated ANSI string.
            let address = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) };
            match address {
                // SAFETY: the library documents this export with exactly
                // the signature named here.
                Some(function) => unsafe {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, $kind>(function)
                },
                None => return Err(Unavailable(format!("ProjectedFSLib.dll lacks {}", $name))),
            }
        }};
    }
    Ok(Api {
        mark: symbol!("PrjMarkDirectoryAsPlaceholder", Mark),
        start: symbol!("PrjStartVirtualizing", Start),
        stop: symbol!("PrjStopVirtualizing", Stop),
        fill: symbol!("PrjFillDirEntryBuffer", FillEntry),
        placeholder: symbol!("PrjWritePlaceholderInfo", WritePlaceholder),
        data: symbol!("PrjWriteFileData", WriteData),
        allocate: symbol!("PrjAllocateAlignedBuffer", Allocate),
        free: symbol!("PrjFreeAlignedBuffer", Free),
        disk_state: symbol!("PrjGetOnDiskFileState", DiskState),
        delete: symbol!("PrjDeleteFile", DeleteItem),
        update: symbol!("PrjUpdateFileIfNeeded", UpdateItem),
        matches: symbol!("PrjFileNameMatch", Match),
        compare: symbol!("PrjFileNameCompare", Compare),
        wild: symbol!("PrjDoesNameContainWildCards", Wild),
    })
}

/// Whether the library is installed; `Err` carries how to enable it.
pub fn available() -> Result<(), Unavailable> {
    api().map(|_| ())
}

/// A NUL-terminated wide copy of `text`.
pub fn wide(text: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain([0]).collect()
}

/// The text of a NUL-terminated wide string; `None` when it is not UTF-16.
///
/// # Safety
/// `pointer` is null or points at a NUL-terminated wide string.
unsafe fn text(pointer: PCWSTR) -> Option<String> {
    if pointer.is_null() {
        return Some(String::new());
    }
    let mut length = 0;
    // SAFETY: the caller's string is NUL-terminated, so every index up to
    // and including the terminator is readable.
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` units before the terminator are initialized.
    String::from_utf16(unsafe { std::slice::from_raw_parts(pointer, length) }).ok()
}

/// Orders two names as NTFS does, ignoring case. Both end in NUL.
pub fn compare(left: &[u16], right: &[u16]) -> std::cmp::Ordering {
    debug_assert!(left.last() == Some(&0) && right.last() == Some(&0));
    match api() {
        // SAFETY: both slices end in NUL and outlive the call.
        Ok(api) => unsafe { (api.compare)(left.as_ptr(), right.as_ptr()) }.cmp(&0),
        Err(_) => left.cmp(right),
    }
}

/// Basic facts of one projected item. Times are 100 ns ticks since 1601.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BasicInfo {
    pub directory: bool,
    pub size: u64,
    pub written: i64,
    pub attributes: u32,
}

impl BasicInfo {
    fn raw(&self) -> prj::PRJ_FILE_BASIC_INFO {
        prj::PRJ_FILE_BASIC_INFO {
            IsDirectory: self.directory,
            FileSize: self.size as i64,
            CreationTime: self.written,
            LastAccessTime: self.written,
            LastWriteTime: self.written,
            ChangeTime: self.written,
            FileAttributes: self.attributes,
        }
    }
    fn placeholder(&self, content: &[u8]) -> prj::PRJ_PLACEHOLDER_INFO {
        let mut info = prj::PRJ_PLACEHOLDER_INFO {
            FileBasicInfo: self.raw(),
            ..Default::default()
        };
        let length = content.len().min(info.VersionInfo.ContentID.len());
        info.VersionInfo.ContentID[..length].copy_from_slice(&content[..length]);
        info
    }
}

/// What a placeholder request resolved to.
pub struct Placeholder {
    /// The item's path below the root, in the source's own spelling,
    /// separated by `\`.
    pub path: String,
    pub info: BasicInfo,
    /// The source's content identity, at most 128 bytes; handed back with
    /// every later data request for this placeholder.
    pub content: Vec<u8>,
}

/// One change the file system reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Notification {
    Created {
        path: String,
        directory: bool,
    },
    Overwritten {
        path: String,
    },
    ConvertingToFull {
        path: String,
    },
    Modified {
        path: String,
    },
    Deleted {
        path: String,
        directory: bool,
    },
    PreDelete {
        path: String,
        directory: bool,
    },
    PreRename {
        from: String,
        to: String,
        directory: bool,
    },
    Renamed {
        from: String,
        to: String,
        directory: bool,
    },
    PreHardLink {
        path: String,
        link: String,
    },
    HardLinked {
        path: String,
        link: String,
    },
}

/// Whether a directory entry fitted the request's buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fill {
    Added,
    Full,
}

/// The provider's side of one virtualization instance. `own` is true when
/// the request comes from the provider's own process.
pub trait Handler: Send + Sync + 'static {
    fn start_enumeration(&self, id: u128, path: &str, own: bool) -> Result<(), Hresult>;
    /// Offers entries in order through `fill` until it answers `Full` or
    /// the directory ends. `pattern` ends in NUL.
    fn enumerate(
        &self,
        id: u128,
        restart: bool,
        pattern: Option<&[u16]>,
        fill: &mut dyn FnMut(&[u16], &BasicInfo) -> Fill,
    ) -> Result<(), Hresult>;
    fn end_enumeration(&self, id: u128);
    fn placeholder(&self, path: &str, own: bool) -> Result<Placeholder, Hresult>;
    /// Fills `buffer` with the item's bytes from `offset`; fewer than
    /// `buffer.len()` bytes is a failed read.
    fn read(
        &self,
        path: &str,
        content: &[u8],
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), Hresult>;
    /// An `Err` denies a pre-operation notification.
    fn notify(&self, notification: Notification) -> Result<(), Hresult>;
}

/// Whether `name` matches a search `pattern`; both end in NUL.
pub fn matches(name: &[u16], pattern: &[u16]) -> bool {
    match api() {
        // SAFETY: both slices end in NUL and outlive the calls.
        Ok(api) => unsafe {
            if (api.wild)(pattern.as_ptr()) != 0 {
                (api.matches)(name.as_ptr(), pattern.as_ptr()) != 0
            } else {
                (api.compare)(name.as_ptr(), pattern.as_ptr()) == 0
            }
        },
        Err(_) => false,
    }
}

fn guid(value: u128) -> GUID {
    GUID::from_u128(value)
}
fn number(value: &GUID) -> u128 {
    (u128::from(value.data1) << 96)
        | (u128::from(value.data2) << 80)
        | (u128::from(value.data3) << 64)
        | u128::from(u64::from_be_bytes(value.data4))
}

/// Marks the existing directory `root` as the virtualization root of
/// `instance`. A root that is already marked is refused by the library.
pub fn mark_root(root: &Path, instance: u128) -> Result<(), Error> {
    let api = api().map_err(Error::Unavailable)?;
    let path = wide(root);
    // SAFETY: `path` ends in NUL; the identity is read during the call.
    let result = unsafe {
        (api.mark)(
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            &guid(instance),
        )
    };
    check("PrjMarkDirectoryAsPlaceholder", result)
}

/// A failed library call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Unavailable(Unavailable),
    Call {
        function: &'static str,
        result: Hresult,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "ProjFS unavailable: {reason}"),
            Self::Call { function, result } => {
                let text = std::io::Error::from_raw_os_error(*result);
                write!(f, "{function} failed: 0x{:08X} ({text})", *result as u32)
            }
        }
    }
}
impl std::error::Error for Error {}

fn check(function: &'static str, result: Hresult) -> Result<(), Error> {
    if result >= 0 {
        Ok(())
    } else {
        Err(Error::Call { function, result })
    }
}

/// One running virtualization instance. Dropping it stops it.
pub struct Instance {
    context: Context,
    handler: *mut Arc<dyn Handler>,
}
// SAFETY: the library's context may be used from any thread, and the boxed
// handler is `Send + Sync`; it is freed only after the instance has stopped.
unsafe impl Send for Instance {}
unsafe impl Sync for Instance {}

impl Instance {
    /// Starts serving `root` through `handler` on `threads` worker threads
    /// (0 lets the library choose).
    pub fn start(root: &Path, handler: Arc<dyn Handler>, threads: u32) -> Result<Self, Error> {
        let api = api().map_err(Error::Unavailable)?;
        let path = wide(root);
        let callbacks = prj::PRJ_CALLBACKS {
            StartDirectoryEnumerationCallback: Some(start_enumeration),
            EndDirectoryEnumerationCallback: Some(end_enumeration),
            GetDirectoryEnumerationCallback: Some(get_enumeration),
            GetPlaceholderInfoCallback: Some(get_placeholder),
            GetFileDataCallback: Some(get_data),
            QueryFileNameCallback: None,
            NotificationCallback: Some(notification),
            CancelCommandCallback: None,
        };
        let everything = [0u16];
        let mut mappings = [prj::PRJ_NOTIFICATION_MAPPING {
            NotificationBitMask: NOTIFICATIONS,
            NotificationRoot: everything.as_ptr(),
        }];
        let options = prj::PRJ_STARTVIRTUALIZING_OPTIONS {
            Flags: prj::PRJ_FLAG_NONE,
            PoolThreadCount: threads,
            ConcurrentThreadCount: threads,
            NotificationMappings: mappings.as_mut_ptr(),
            NotificationMappingsCount: 1,
        };
        let handler = Box::into_raw(Box::new(handler));
        let mut context: Context = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call; the library copies
        // the callbacks and options. `handler` stays allocated until `Drop`,
        // which runs after the library has stopped calling back.
        let result = unsafe {
            (api.start)(
                path.as_ptr(),
                &callbacks,
                handler.cast(),
                &options,
                &mut context,
            )
        };
        if let Err(error) = check("PrjStartVirtualizing", result) {
            // SAFETY: the library kept no reference to a failed start's context.
            drop(unsafe { Box::from_raw(handler) });
            return Err(error);
        }
        Ok(Self { context, handler })
    }

    /// Removes an item from the local file system, as the flags allow.
    /// `Ok(false)` when nothing is on disk at `path`.
    pub fn delete_item(&self, path: &str, flags: i32) -> Result<bool, (Error, i32)> {
        let api = api().map_err(|e| (Error::Unavailable(e), 0))?;
        let (name, mut cause) = (wide(path), 0);
        // SAFETY: the context is live, `name` ends in NUL, `cause` is writable.
        let result = unsafe { (api.delete)(self.context, name.as_ptr(), flags, &mut cause) };
        if result == NOT_FOUND || result == PATH_NOT_FOUND {
            return Ok(false);
        }
        check("PrjDeleteFile", result)
            .map(|()| true)
            .map_err(|e| (e, cause))
    }

    /// Replaces an on-disk item's placeholder facts and content identity,
    /// as the flags allow; a full file becomes a placeholder again.
    /// `Ok(false)` when nothing is on disk at `path`.
    pub fn update_item(
        &self,
        path: &str,
        info: &BasicInfo,
        content: &[u8],
        flags: i32,
    ) -> Result<bool, (Error, i32)> {
        let api = api().map_err(|e| (Error::Unavailable(e), 0))?;
        let (name, mut cause) = (wide(path), 0);
        let placeholder = info.placeholder(content);
        // SAFETY: the context is live and every pointer is valid for the call.
        let result = unsafe {
            (api.update)(
                self.context,
                name.as_ptr(),
                &placeholder,
                std::mem::size_of::<prj::PRJ_PLACEHOLDER_INFO>() as u32,
                flags,
                &mut cause,
            )
        };
        if result == NOT_FOUND || result == PATH_NOT_FOUND {
            return Ok(false);
        }
        check("PrjUpdateFileIfNeeded", result)
            .map(|()| true)
            .map_err(|e| (e, cause))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Ok(api) = api() {
            // SAFETY: the context came from a successful start and is
            // stopped once. The call returns after running callbacks end.
            unsafe { (api.stop)(self.context) };
        }
        // SAFETY: no callback can run after the stop, so this is the only
        // reference to the boxed handler.
        drop(unsafe { Box::from_raw(self.handler) });
    }
}

/// The on-disk state bits of the item at `path`; `None` when nothing is
/// on disk there.
pub fn disk_state(path: &Path) -> Result<Option<i32>, Error> {
    let api = api().map_err(Error::Unavailable)?;
    let (name, mut bits) = (wide(path), 0);
    // SAFETY: `name` ends in NUL and `bits` is writable.
    let result = unsafe { (api.disk_state)(name.as_ptr(), &mut bits) };
    if result == NOT_FOUND || result == PATH_NOT_FOUND {
        return Ok(None);
    }
    check("PrjGetOnDiskFileState", result).map(|()| Some(bits))
}

/// A file's volume, index, link count and attributes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identity {
    pub volume: u32,
    pub index: u64,
    pub links: u32,
    pub attributes: u32,
}

/// Reads `file`'s identity from its open handle.
pub fn identity(file: &std::fs::File) -> std::io::Result<Identity> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open for the borrow's duration and `info` is
    // writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(Identity {
        volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        links: info.nNumberOfLinks,
        attributes: info.dwFileAttributes,
    })
}

struct Request<'a> {
    handler: &'a dyn Handler,
    context: Context,
    path: String,
    own: bool,
    restart: bool,
    stream: GUID,
    content: [u8; 128],
}

/// Reads one callback's data and runs `serve`, answering a panic or a path
/// that is not UTF-16 as a failed request.
///
/// # Safety
/// `data` is the callback data the library passed to a callback of an
/// instance this module started, and the callback has not returned.
unsafe fn serve(
    data: *const prj::PRJ_CALLBACK_DATA,
    serve: impl FnOnce(Request<'_>) -> Result<(), Hresult>,
) -> HRESULT {
    // SAFETY: the library passes valid callback data for the callback's duration.
    let data = unsafe { &*data };
    // SAFETY: the instance context is the boxed handler `Instance::start`
    // supplied; it outlives every callback.
    let handler = unsafe { &*data.InstanceContext.cast::<Arc<dyn Handler>>() };
    // SAFETY: the path is a NUL-terminated wide string for the callback's duration.
    let Some(path) = (unsafe { text(data.FilePathName) }) else {
        return INVALID_NAME;
    };
    let content = if data.VersionInfo.is_null() {
        [0; 128]
    } else {
        // SAFETY: a non-null version info is valid for the callback's duration.
        unsafe { (*data.VersionInfo).ContentID }
    };
    let request = Request {
        handler: handler.as_ref(),
        context: data.NamespaceVirtualizationContext,
        path,
        own: data.TriggeringProcessId == std::process::id(),
        restart: data.Flags & prj::PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN != 0,
        stream: data.DataStreamId,
        content,
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(request))) {
        Ok(Ok(())) => 0,
        Ok(Err(result)) => result,
        Err(panic) => {
            let text = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("no message");
            eprintln!("layerfs-projfs: a provider callback panicked: {text}");
            E_FAIL
        }
    }
}

unsafe extern "system" fn start_enumeration(
    data: *const prj::PRJ_CALLBACK_DATA,
    id: *const GUID,
) -> HRESULT {
    // SAFETY: called by the library with valid data and identity.
    unsafe {
        let id = number(&*id);
        serve(data, |r| r.handler.start_enumeration(id, &r.path, r.own))
    }
}

unsafe extern "system" fn end_enumeration(
    data: *const prj::PRJ_CALLBACK_DATA,
    id: *const GUID,
) -> HRESULT {
    // SAFETY: called by the library with valid data and identity.
    unsafe {
        let id = number(&*id);
        serve(data, |r| {
            r.handler.end_enumeration(id);
            Ok(())
        })
    }
}

unsafe extern "system" fn get_enumeration(
    data: *const prj::PRJ_CALLBACK_DATA,
    id: *const GUID,
    pattern: PCWSTR,
    buffer: prj::PRJ_DIR_ENTRY_BUFFER_HANDLE,
) -> HRESULT {
    let Ok(api) = api() else { return E_FAIL };
    // SAFETY: called by the library with valid data, identity and buffer;
    // `pattern` is null or NUL-terminated.
    unsafe {
        let id = number(&*id);
        let pattern = if pattern.is_null() || *pattern == 0 {
            None
        } else {
            let mut length = 0;
            while *pattern.add(length) != 0 {
                length += 1;
            }
            Some(std::slice::from_raw_parts(pattern, length + 1))
        };
        serve(data, |r| {
            let mut refused = None;
            let mut added = 0u32;
            r.handler
                .enumerate(id, r.restart, pattern, &mut |name, info| {
                    let result = (api.fill)(name.as_ptr(), &info.raw(), buffer);
                    if result >= 0 {
                        added += 1;
                        Fill::Added
                    } else {
                        if result != INSUFFICIENT_BUFFER || added == 0 {
                            refused = Some(result);
                        }
                        Fill::Full
                    }
                })?;
            refused.map_or(Ok(()), Err)
        })
    }
}

unsafe extern "system" fn get_placeholder(data: *const prj::PRJ_CALLBACK_DATA) -> HRESULT {
    let Ok(api) = api() else { return E_FAIL };
    // SAFETY: called by the library with valid data.
    unsafe {
        serve(data, |r| {
            let found = r.handler.placeholder(&r.path, r.own)?;
            let name = wide(&found.path);
            let info = found.info.placeholder(&found.content);
            let result = (api.placeholder)(
                r.context,
                name.as_ptr(),
                &info,
                std::mem::size_of::<prj::PRJ_PLACEHOLDER_INFO>() as u32,
            );
            if result >= 0 {
                Ok(())
            } else {
                Err(result)
            }
        })
    }
}

/// Bytes one data write carries: a multiple of every sector size.
const WINDOW: usize = 1 << 20;

unsafe extern "system" fn get_data(
    data: *const prj::PRJ_CALLBACK_DATA,
    offset: u64,
    length: u32,
) -> HRESULT {
    let Ok(api) = api() else { return E_FAIL };
    // SAFETY: called by the library with valid data; the aligned buffer is
    // allocated, wholly initialized before it is read, and freed here.
    unsafe {
        serve(data, |r| {
            let window = WINDOW.min(length as usize).max(1);
            let memory = (api.allocate)(r.context, window);
            if memory.is_null() {
                return Err(win32(14));
            }
            std::ptr::write_bytes(memory.cast::<u8>(), 0, window);
            let buffer = std::slice::from_raw_parts_mut(memory.cast::<u8>(), window);
            let (mut at, end) = (offset, offset + u64::from(length));
            let mut result = Ok(());
            while at < end && result.is_ok() {
                let step = ((end - at) as usize).min(window);
                result = r
                    .handler
                    .read(&r.path, &r.content, at, &mut buffer[..step])
                    .and_then(|()| {
                        let written = (api.data)(r.context, &r.stream, memory, at, step as u32);
                        if written >= 0 {
                            Ok(())
                        } else {
                            Err(written)
                        }
                    });
                at += step as u64;
            }
            (api.free)(memory);
            result
        })
    }
}

unsafe extern "system" fn notification(
    data: *const prj::PRJ_CALLBACK_DATA,
    directory: bool,
    kind: prj::PRJ_NOTIFICATION,
    destination: PCWSTR,
    parameters: *mut prj::PRJ_NOTIFICATION_PARAMETERS,
) -> HRESULT {
    // SAFETY: called by the library with valid data; `destination` is null
    // or NUL-terminated; `parameters` is null or writable.
    unsafe {
        let Some(to) = text(destination) else {
            return INVALID_NAME;
        };
        serve(data, |r| {
            let path = r.path;
            let event = match kind {
                prj::PRJ_NOTIFICATION_NEW_FILE_CREATED => Notification::Created { path, directory },
                prj::PRJ_NOTIFICATION_FILE_OVERWRITTEN => Notification::Overwritten { path },
                prj::PRJ_NOTIFICATION_FILE_PRE_CONVERT_TO_FULL => {
                    Notification::ConvertingToFull { path }
                }
                prj::PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_MODIFIED => {
                    Notification::Modified { path }
                }
                prj::PRJ_NOTIFICATION_FILE_HANDLE_CLOSED_FILE_DELETED => {
                    Notification::Deleted { path, directory }
                }
                prj::PRJ_NOTIFICATION_PRE_DELETE => Notification::PreDelete { path, directory },
                prj::PRJ_NOTIFICATION_PRE_RENAME => Notification::PreRename {
                    from: path,
                    to,
                    directory,
                },
                prj::PRJ_NOTIFICATION_FILE_RENAMED => Notification::Renamed {
                    from: path,
                    to,
                    directory,
                },
                prj::PRJ_NOTIFICATION_PRE_SET_HARDLINK => {
                    Notification::PreHardLink { path, link: to }
                }
                prj::PRJ_NOTIFICATION_HARDLINK_CREATED => {
                    Notification::HardLinked { path, link: to }
                }
                _ => return Ok(()),
            };
            let keeps_mask = matches!(
                kind,
                prj::PRJ_NOTIFICATION_NEW_FILE_CREATED
                    | prj::PRJ_NOTIFICATION_FILE_RENAMED
                    | prj::PRJ_NOTIFICATION_HARDLINK_CREATED
            );
            if keeps_mask && !parameters.is_null() {
                // The three union arms that carry a mask share its layout.
                (*parameters).PostCreate.NotificationMask = prj::PRJ_NOTIFY_USE_EXISTING_MASK;
            }
            r.handler.notify(event)
        })
    }
}
