//! Capture: brings the bound engine Workspace's view to what the local
//! disk holds at every journaled path, so the existing Commit can publish it.
//!
//! The engine Workspace (its SQLite overlay over the immutable base) is the
//! Commit's input, exactly as for a FUSE mount. Here NTFS was the live
//! write layer, so before a Commit each changed path is compared between
//! the local disk and the Workspace view and the difference is applied to
//! the view through the engine's own mutation operations. The step is
//! idempotent: a path whose view already equals the disk costs a compare.
use crate::provider::tree::{join, names};
use crate::provider::Provider;
use crate::source::{attribute, unix};
use crate::sys;
use crate::track::journal::{Recorded, Rename};
use crate::track::local::{native, Local, LocalItem};
use layerfs_bridge::control::SdkRefusal;
use layerfs_content::filesystem::PathName;
use layerfs_content::object::inode_leaf::InodeKind;
use layerfs_store_service::store::BoundWorkspace;
use layerfs_store_service::write::mutation::{read_failure, Mutation};
use layerfs_workspace::{Operation, Time, ViewStat};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::path::Path;

/// Bytes one overlay write carries: one engine record at most.
const PIECE: usize = 32 * 1024;
const FILE_SHARE_READ: u32 = 1;
const SHARING_VIOLATION: i32 = 32;

/// What one capture did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Captured {
    /// Paths whose view was changed.
    pub applied: u64,
    /// File bytes read from the local disk.
    pub bytes_read: u64,
    /// File bytes written into the Workspace overlay.
    pub bytes_written: u64,
    /// Files another process held open for writing while they were read.
    /// Their bytes did not change during the read; they stay journaled.
    pub live: Vec<String>,
}

/// Why a capture stopped. Nothing was published.
#[derive(Debug)]
pub enum CaptureError {
    Local(std::io::Error),
    ProjFs(sys::Error),
    /// The engine refused a change; its code and message are the SDK's.
    Store(SdkRefusal),
    /// An engine outcome is uncertain; the Workspace keeps its custody.
    Unknown(SdkRefusal),
    /// A file changed while it was being read.
    Unstable(String),
    /// Another process holds the file open and lets no one read it.
    Locked(String),
    /// Paths whose names the Store cannot hold, each with the reason.
    Unrepresentable(Vec<(String, String)>),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(error) => write!(f, "Failed: local disk: {error}"),
            Self::ProjFs(error) => write!(f, "Failed: {error}"),
            Self::Store(refusal) => write!(f, "{}: {}", refusal.code, refusal.message),
            Self::Unknown(refusal) => write!(f, "Unknown: {}", refusal.message),
            Self::Unstable(path) => write!(f, "Busy: {path} changed while it was read"),
            Self::Locked(path) => {
                write!(
                    f,
                    "Busy: {path} is held open exclusively by another process"
                )
            }
            Self::Unrepresentable(paths) => {
                write!(
                    f,
                    "Unsupported: {} name(s) the Store cannot hold",
                    paths.len()
                )
            }
        }
    }
}
impl std::error::Error for CaptureError {}
impl From<std::io::Error> for CaptureError {
    fn from(error: std::io::Error) -> Self {
        Self::Local(error)
    }
}
impl From<sys::Error> for CaptureError {
    fn from(error: sys::Error) -> Self {
        Self::ProjFs(error)
    }
}
impl From<SdkRefusal> for CaptureError {
    fn from(refusal: SdkRefusal) -> Self {
        if refusal.code == "Unknown" {
            Self::Unknown(refusal)
        } else {
            Self::Store(refusal)
        }
    }
}

fn absolute(path: &str) -> String {
    format!("/{path}")
}

/// The view entry a typed path names, matching each name exactly first and
/// then as NTFS compares names, with the view's own spelling.
pub fn view_find(
    mutation: &Mutation<'_>,
    typed: &str,
) -> Result<Option<(String, ViewStat)>, SdkRefusal> {
    let mut at = String::new();
    let mut stat = mutation
        .view
        .stat(mutation.op.overlay(), mutation.view.root_serial())
        .map_err(read_failure)?;
    for name in names(typed) {
        if stat.kind != InodeKind::Directory {
            return Ok(None);
        }
        let exact = join(&at, name);
        let found = match PathName::new(name) {
            Ok(_) => mutation.optional(&absolute(&exact))?,
            Err(_) => None,
        };
        if let Some(found) = found {
            (at, stat) = (exact, found);
            continue;
        }
        let wanted = sys::wide(name);
        let (mut after, mut matched) = (None::<Vec<u8>>, None);
        'pages: loop {
            let page = mutation
                .view
                .list(mutation.op.overlay(), stat.serial, after.as_deref())
                .map_err(read_failure)?;
            for (bytes, serial) in &page.entries {
                let spelled = String::from_utf8_lossy(bytes);
                if sys::compare(&sys::wide(spelled.as_ref()), &wanted).is_eq() {
                    matched = Some((spelled.into_owned(), *serial));
                    break 'pages;
                }
            }
            match page.continuation {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        let Some((spelled, serial)) = matched else {
            return Ok(None);
        };
        at = join(&at, &spelled);
        stat = mutation
            .view
            .stat(mutation.op.overlay(), serial)
            .map_err(read_failure)?;
    }
    Ok(Some((at, stat)))
}

struct Capture<'a, 'w> {
    mutation: Mutation<'w>,
    local: Local,
    root: &'a Path,
    done: Captured,
    refused: Vec<(String, String)>,
}

impl Capture<'_, '_> {
    /// Whether every name of `path` is one the Store can hold.
    fn representable(&mut self, path: &str) -> bool {
        for name in names(path) {
            if let Err(error) = PathName::new(name) {
                self.refused.push((path.to_owned(), error.to_string()));
                return false;
            }
        }
        true
    }

    fn ensure_parent(&mut self, path: &str) -> Result<(), CaptureError> {
        if let Some((parent, _)) = path.rsplit_once('/') {
            if self.mutation.optional(&absolute(parent))?.is_none() {
                self.mutation.mkdir(&absolute(parent), None)?;
            }
        }
        Ok(())
    }

    /// Replays a rename in the view when the disk shows it happened, so the
    /// entry keeps its identity instead of becoming a delete and an add.
    fn rename(&mut self, rename: &Rename) -> Result<(), CaptureError> {
        let Some((to, _)) = self.local.find(&rename.to)? else {
            return Ok(());
        };
        if self.local.find(&rename.from)?.is_some() || !self.representable(&to) {
            return Ok(());
        }
        let Some((from, _)) = view_find(&self.mutation, &rename.from)? else {
            return Ok(());
        };
        match view_find(&self.mutation, &to)? {
            // Only the spelling's case changed: the view finds the old name.
            Some((existing, _)) if existing == from => {}
            Some(_) => return Ok(()),
            None => {}
        }
        if from == to {
            return Ok(());
        }
        self.ensure_parent(&to)?;
        self.mutation.rename(&absolute(&from), &absolute(&to))?;
        self.done.applied += 1;
        Ok(())
    }

    /// Replays a hard link in the view when both names are one file on disk.
    fn link(&mut self, path: &str, link: &str) -> Result<(), CaptureError> {
        let (Some((path, _)), Some((link, _))) = (self.local.find(path)?, self.local.find(link)?)
        else {
            return Ok(());
        };
        let same = |left: &str, right: &str| -> std::io::Result<bool> {
            let left = sys::identity(&std::fs::File::open(native(self.root, left))?)?;
            let right = sys::identity(&std::fs::File::open(native(self.root, right))?)?;
            Ok(left.volume == right.volume && left.index == right.index)
        };
        if !same(&path, &link)? || !self.representable(&link) {
            return Ok(());
        }
        // The first name's bytes reach the view before the second is bound.
        if let Some((_, item)) = self.local.find(&path)? {
            self.item(&path, &item)?;
        }
        let Some((_, target)) = view_find(&self.mutation, &path)? else {
            return Ok(());
        };
        if target.kind != InodeKind::RegularFile || view_find(&self.mutation, &link)?.is_some() {
            return Ok(());
        }
        self.ensure_parent(&link)?;
        let (parent, name) = self.mutation.parent(&absolute(&link))?;
        self.mutation.apply(Operation::Link {
            serial: target.serial,
            parent,
            name,
        })?;
        self.done.applied += 1;
        Ok(())
    }

    fn path(&mut self, typed: &str) -> Result<(), CaptureError> {
        match self.local.find(typed)? {
            Some((path, item)) => self.item(&path, &item),
            None => {
                if let Some((gone, _)) = view_find(&self.mutation, typed)? {
                    self.mutation.remove(&absolute(&gone))?;
                    self.done.applied += 1;
                }
                Ok(())
            }
        }
    }

    fn item(&mut self, path: &str, item: &LocalItem) -> Result<(), CaptureError> {
        if !self.representable(path) {
            return Ok(());
        }
        let mut existing = None;
        if let Some((spelled, stat)) = view_find(&self.mutation, path)? {
            let folder = stat.kind == InodeKind::Directory;
            if folder != item.directory || stat.kind == InodeKind::Symlink {
                self.mutation.remove(&absolute(&spelled))?;
                self.done.applied += 1;
            } else {
                if spelled != path {
                    // The same name in another case: a case-only rename.
                    self.mutation.rename(&absolute(&spelled), &absolute(path))?;
                    self.done.applied += 1;
                }
                existing = Some(stat);
            }
        }
        if !item.directory {
            return self.file(path, item, existing);
        }
        if existing.is_none() {
            self.mutation.mkdir(&absolute(path), None)?;
            self.done.applied += 1;
            // A directory new to the view arrives with everything in it.
            for child in self.local.list(path)?.unwrap_or_default() {
                self.item(&join(path, &child.name), &child)?;
            }
        }
        Ok(())
    }

    fn file(
        &mut self,
        path: &str,
        item: &LocalItem,
        existing: Option<ViewStat>,
    ) -> Result<(), CaptureError> {
        let read_only = item.attributes & attribute::READ_ONLY != 0;
        let wanted = |mode: u32| match (read_only, mode & 0o222 == 0) {
            (true, false) => mode & !0o222,
            (false, true) => mode | 0o200,
            _ => mode,
        };
        // A placeholder, hydrated or not, holds the base's bytes: only a
        // full file's bytes can differ from the view.
        let full = self
            .local
            .state(path)?
            .is_none_or(|bits| bits & sys::state::FULL != 0);
        let (serial, mode, changed) = match existing {
            Some(stat) => {
                let changed = full && self.bytes(path, stat.serial, Some(stat.logical_len))?;
                (stat.serial, stat.metadata.mode, changed)
            }
            None => {
                self.ensure_parent(path)?;
                let (parent, name) = self.mutation.parent(&absolute(path))?;
                let mode = wanted(0o644);
                let serial = self
                    .mutation
                    .apply(Operation::Create { parent, name, mode })?
                    .map(|stat| stat.serial)
                    .ok_or_else(|| SdkRefusal {
                        code: "Failed".to_owned(),
                        message: "the created file has no inode".to_owned(),
                        detail: layerfs_bridge::control::SdkValue::Null,
                    })?;
                self.bytes(path, serial, None)?;
                (serial, mode, true)
            }
        };
        let (seconds, nanoseconds) = unix(item.written);
        if changed || wanted(mode) != mode {
            self.mutation.apply(Operation::SetAttributes {
                serial,
                mode: (wanted(mode) != mode).then(|| wanted(mode)),
                mtime: changed.then_some(Time {
                    seconds,
                    nanoseconds,
                }),
                size: None,
            })?;
            self.done.applied += 1;
        }
        Ok(())
    }

    /// Makes the view file `serial` hold the local file's bytes; true when
    /// it wrote any. `held` is the view file's length when it has one.
    fn bytes(&mut self, path: &str, serial: u64, held: Option<u64>) -> Result<bool, CaptureError> {
        let native = native(self.root, path);
        // Deny writers for the read. A file someone already writes cannot
        // be opened this way: it is read shared and checked for change.
        let (file, live) = match std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&native)
        {
            Ok(file) => (file, false),
            Err(error) if error.raw_os_error() == Some(SHARING_VIOLATION) => {
                match std::fs::File::open(&native) {
                    Ok(file) => (file, true),
                    Err(error) if error.raw_os_error() == Some(SHARING_VIOLATION) => {
                        return Err(CaptureError::Locked(path.to_owned()));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        };
        let stamp = |file: &std::fs::File| -> std::io::Result<(u64, std::time::SystemTime)> {
            let metadata = file.metadata()?;
            Ok((metadata.len(), metadata.modified()?))
        };
        let before = stamp(&file)?;
        let length = before.0;
        let same_length = held == Some(length);
        let (mut buffer, mut offset, mut changed) = (vec![0u8; PIECE], 0u64, false);
        while offset < length {
            let step = ((length - offset) as usize).min(PIECE);
            let mut filled = 0;
            while filled < step {
                let read = file.seek_read(&mut buffer[filled..step], offset + filled as u64)?;
                if read == 0 {
                    return Err(CaptureError::Unstable(path.to_owned()));
                }
                filled += read;
            }
            self.done.bytes_read += step as u64;
            let piece = &buffer[..step];
            if !same_length || self.mutation.read(serial, offset, step as u32)? != piece {
                self.mutation.write_at(serial, offset, piece)?;
                self.done.bytes_written += step as u64;
                changed = true;
            }
            offset += step as u64;
        }
        if !same_length {
            self.mutation.apply(Operation::SetAttributes {
                serial,
                mode: None,
                mtime: None,
                size: Some(length),
            })?;
            changed = true;
        }
        if live {
            if stamp(&file)? != before {
                return Err(CaptureError::Unstable(path.to_owned()));
            }
            self.done.live.push(path.to_owned());
        }
        Ok(changed)
    }
}

/// Applies every journaled change to the bound Workspace's view. On an
/// error nothing is published; an uncertain engine outcome keeps the
/// Workspace's custody untouched.
pub fn capture(
    bound: &BoundWorkspace,
    provider: &Provider,
    root: &Path,
    recorded: &Recorded,
) -> Result<Captured, CaptureError> {
    let _local = provider.local_only();
    let mut capture = Capture {
        mutation: Mutation::new(bound)?,
        local: Local::new(root),
        root,
        done: Captured::default(),
        refused: Vec::new(),
    };
    let result = (|| {
        for rename in &recorded.renames {
            capture.rename(rename)?;
        }
        for link in &recorded.links {
            capture.link(&link.path, &link.link)?;
        }
        for typed in recorded.touched.keys() {
            capture.path(typed)?;
        }
        Ok(())
    })();
    let Capture {
        mutation,
        done,
        refused,
        ..
    } = capture;
    if let Err(CaptureError::Unknown(refusal)) = result {
        // The engine's custody of an uncertain job is not released.
        return Err(CaptureError::Unknown(refusal));
    }
    mutation.finish()?;
    result?;
    if !refused.is_empty() {
        return Err(CaptureError::Unrepresentable(refused));
    }
    Ok(done)
}
