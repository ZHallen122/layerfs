//! Bringing the directory from the tree it shows to the tree a merge
//! installed.
//!
//! A merge changes paths this Workspace did not change, and files it did.
//! The engine installs the result in the Workspace view; with a FUSE mount
//! that is all, the view being what the directory shows. Here the directory
//! is NTFS, so every item on disk that the merge changed has to be brought
//! over:
//!
//! - a placeholder is given the new facts and content, or removed;
//! - a full file that still holds what this Workspace captured is rewritten
//!   with the merged bytes, or removed;
//! - a name the merge added below a directory made in this Workspace, into
//!   which ProjFS projects nothing, is written to the disk;
//! - a directory the merge removed is removed once it is empty.
//!
//! Whether a full file "still holds what this Workspace captured" is the
//! one question that must not be guessed. The record of committed files
//! answers it for a file whose size, time and change journal number are the
//! ones the capture saw; for any other file the bytes are compared with the
//! tree the directory showed. A file that holds something else changed
//! after the capture read it: it is left alone and named.
//!
//! An item that cannot be brought over (another process holds the file,
//! ProjFS will not update the placeholder) is reported as refused. The
//! caller keeps such paths (`unfollowed.rs`) and has them tried again by
//! [`retry`]; this step leaves a path alone that is already on that list.
//!
//! Work follows the items on disk, never the tree.
use super::ledger::{Ledger, Stored};
use super::mount::{Workspace, WorkspaceError};
use super::unfollowed::Unfollowed;
use crate::ntfs::Sessions;
use crate::provider::serve::basic_info;
use crate::provider::tree::{join, Directory, Found, Tree};
use crate::source::{attribute, Entry, Source, SourceError};
use crate::sys;
use crate::track::local::{native, Local, LocalItem, Names};
use crate::track::status::bytes_differ;
use std::io::Write;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;

/// Bytes copied per step.
const WINDOW: usize = 1 << 20;
const SHARING_VIOLATION: i32 = 32;
const SHARE_ALL: u32 = 7;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// On-disk items brought to a new base.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Rebased {
    /// On-disk placeholders given the new base's facts and content.
    pub updated: u64,
    /// On-disk placeholders, and emptied directories, removed because the
    /// new base has no such item.
    pub removed: u64,
    /// Full files that held what this Workspace captured and that the merge
    /// changed: rewritten with the merged bytes, or removed.
    pub replaced: u64,
    /// Files the merge added below a directory ProjFS projects nothing
    /// into: written to the disk.
    pub written: u64,
    /// Full files the merge changed that no longer hold what the capture
    /// read: left as they are. They differ from the Branch, and the next
    /// Commit compares them.
    pub kept: Vec<String>,
    /// On-disk items that could not be changed, each with the reason.
    pub refused: Vec<(String, String)>,
}

/// The directory `typed` of `tree`, with the tree's spelling of its path.
fn directory(tree: &Tree, typed: &str) -> Result<Option<(String, Arc<Directory>)>, SourceError> {
    let path = match tree.resolve(typed)? {
        Some(found) if found.is_directory() => found.path().to_owned(),
        _ => return Ok(None),
    };
    match tree.directory(&path) {
        Ok(directory) => Ok(Some((path, directory))),
        Err(SourceError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

/// What the file at `native` is now, for the record of committed files;
/// `None` when its changes are not closed in the journal (another process
/// has it open), so that no record can vouch for it.
fn stamp(native: &Path, sessions: Option<&Sessions>) -> std::io::Result<Option<Stored>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .custom_flags(OPEN_REPARSE_POINT)
        .open(native)?;
    let metadata = file.metadata()?;
    Ok(sessions
        .and_then(|sessions| sessions.settled(&file))
        .map(|change| Stored {
            size: metadata.len(),
            written: metadata.last_write_time() as i64,
            change,
        }))
}

fn read_only(native: &Path, on: bool) -> std::io::Result<()> {
    let mut permissions = std::fs::symlink_metadata(native)?.permissions();
    if permissions.readonly() != on {
        permissions.set_readonly(on);
        std::fs::set_permissions(native, permissions)?;
    }
    Ok(())
}

/// Why a file could not be written, and whether it was changed at all.
struct Unwritten {
    error: std::io::Error,
    /// The file was opened for the new bytes and may hold a part of them.
    touched: bool,
}

/// Fills the file at `path` with the bytes `source` holds for `entry`.
fn fill(path: &Path, source: &dyn Source, listed: &str, entry: &Entry) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    let mut buffer = vec![0u8; WINDOW.min(entry.size as usize).max(1)];
    let mut offset = 0u64;
    while offset < entry.size {
        let step = ((entry.size - offset) as usize).min(buffer.len());
        source
            .read(listed, &entry.content, offset, &mut buffer[..step])
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        file.write_all(&buffer[..step])?;
        offset += step as u64;
    }
    Ok(())
}

/// Writes the bytes `source` holds for `entry`, listed at `listed`, into the
/// file at `native`, and returns what the file then is.
///
/// The file is written in place, so it stays the same file: a process that
/// has it open or mapped keeps seeing it, and a file that may not be
/// written while it is mapped is refused instead of replaced under the
/// mapping. So that the Store cannot fail halfway through, the bytes are
/// first fetched whole into `scratch`, a file in the state directory.
fn write(
    native: &Path,
    scratch: &Path,
    source: &dyn Source,
    listed: &str,
    entry: &Entry,
    sessions: Option<&Sessions>,
) -> Result<Option<Stored>, Unwritten> {
    let untouched = |error| Unwritten {
        error,
        touched: false,
    };
    if let Some(parent) = native.parent() {
        std::fs::create_dir_all(parent).map_err(untouched)?;
    }
    fill(scratch, source, listed, entry).map_err(|error| {
        let _ = std::fs::remove_file(scratch);
        untouched(error)
    })?;
    let locked =
        std::fs::symlink_metadata(native).is_ok_and(|metadata| metadata.permissions().readonly());
    let copied = (|| {
        if locked {
            read_only(native, false).map_err(untouched)?;
        }
        let mut from = std::fs::File::open(scratch).map_err(untouched)?;
        let mut into = std::fs::File::create(native).map_err(untouched)?;
        std::io::copy(&mut from, &mut into)
            .map(drop)
            .map_err(|error| Unwritten {
                error,
                touched: true,
            })
    })();
    let _ = std::fs::remove_file(scratch);
    if let Err(unwritten) = copied {
        if locked && !unwritten.touched {
            let _ = read_only(native, true);
        }
        return Err(unwritten);
    }
    let finish = || -> std::io::Result<Option<Stored>> {
        if entry.attributes & attribute::READ_ONLY != 0 {
            read_only(native, true)?;
        }
        // Read once the writing handle is closed: the close is then the
        // file's newest change journal record, unless another process has
        // it open.
        stamp(native, sessions)
    };
    // The bytes are in place; without the rest nothing vouches for the file.
    Ok(finish().unwrap_or(None))
}

struct Follow<'a> {
    workspace: &'a Workspace,
    target: &'a Tree,
    /// The sources of the tree the directory showed and of the new one.
    was: Arc<dyn Source>,
    now: Arc<dyn Source>,
    local: Local,
    stamps: &'a mut Ledger,
    sessions: Option<Sessions>,
    done: Rebased,
}

/// Flags for a placeholder: its metadata may have been changed locally.
const PLACEHOLDER: i32 = sys::ALLOW_DIRTY_METADATA | sys::ALLOW_READ_ONLY;
const EVERY: i32 = PLACEHOLDER | sys::ALLOW_DIRTY_DATA | sys::ALLOW_TOMBSTONE;

impl Follow<'_> {
    fn root(&self) -> &Path {
        self.workspace.projection.root()
    }

    /// Where a file's new bytes are written before they are moved in.
    fn scratch(&self) -> std::path::PathBuf {
        self.workspace.projection.state().join("follow.tmp")
    }

    /// Enters a file this step wrote into the record, or takes it out when
    /// nothing can vouch for it.
    fn note(&mut self, path: &str, stored: Option<Stored>) {
        match stored {
            Some(stored) => self.stamps.record(path, stored),
            None => self.stamps.forget(path),
        }
    }

    fn refuse(&mut self, path: &str, why: impl std::fmt::Display) {
        self.stamps.forget(path);
        self.done.refused.push((path.to_owned(), why.to_string()));
    }

    /// Writes the target's `entry`, listed at `listed`, to the disk at
    /// `path`: a file, or a directory with all it holds.
    fn arrive(&mut self, path: &str, listed: &str, entry: &Entry) {
        let mut pending = vec![(path.to_owned(), listed.to_owned(), entry.clone())];
        while let Some((path, listed, entry)) = pending.pop() {
            let native = native(self.root(), &path);
            if !entry.directory {
                match write(
                    &native,
                    &self.scratch(),
                    self.now.as_ref(),
                    &listed,
                    &entry,
                    self.sessions.as_ref(),
                ) {
                    Ok(stored) => {
                        self.note(&path, stored);
                        self.done.written += 1;
                    }
                    Err(unwritten) => {
                        // Nothing stood here before: a part of a file is
                        // worse than none.
                        if unwritten.touched {
                            let _ = std::fs::remove_file(&native);
                        }
                        self.refuse(&path, unwritten.error);
                    }
                }
                continue;
            }
            if let Err(error) = std::fs::create_dir_all(&native) {
                self.refuse(&path, error);
                continue;
            }
            match self.target.directory(&listed) {
                Ok(directory) => pending.extend(directory.items.iter().map(|item| {
                    (
                        join(&path, &item.entry.name),
                        join(&listed, &item.entry.name),
                        item.entry.clone(),
                    )
                })),
                Err(error) => self.refuse(&path, error),
            }
        }
    }

    /// Whether the full file at `path` holds the bytes of `entry`, which
    /// `source` lists at `listed`.
    fn holds(
        &self,
        path: &str,
        item: &LocalItem,
        source: &dyn Source,
        listed: &str,
        entry: &Entry,
    ) -> Result<bool, WorkspaceError> {
        bytes_differ(&self.local, path, item, source, listed, entry)
            .map(|differ| !differ)
            .map_err(|error| WorkspaceError::Failed(format!("baseline update: {path}: {error}")))
    }

    /// Brings the file on disk at `path` from `was` to `now`, the entries
    /// the two trees hold for it, each with its directory's path.
    fn file(
        &mut self,
        path: &str,
        item: &LocalItem,
        was: Option<(&str, &Entry)>,
        now: Option<(&str, &Entry)>,
        projected: bool,
    ) -> Result<(), WorkspaceError> {
        let instance = self.workspace.projection.instance();
        let name = path.replace('/', "\\");
        let state = self
            .local
            .state(path)
            .map_err(|error| WorkspaceError::Failed(format!("baseline update: {error}")))?
            .unwrap_or(sys::state::FULL);
        let placeholder = state & (sys::state::PLACEHOLDER | sys::state::HYDRATED) != 0
            && state & sys::state::FULL == 0;
        let now = now.filter(|(_, entry)| !entry.directory);
        if placeholder {
            // Made after the capture for a path the shown tree lacks: not
            // this step's to judge.
            if was.is_none() {
                return Ok(());
            }
            let done = match now {
                Some((_, now)) => instance
                    .update_item(&name, &basic_info(now), &now.content, PLACEHOLDER)
                    .map(|changed| self.done.updated += u64::from(changed)),
                None => instance
                    .delete_item(&name, PLACEHOLDER)
                    .map(|changed| self.done.removed += u64::from(changed)),
            };
            if let Err((error, cause)) = done {
                self.refuse(path, format!("{error} (reason bits {cause:#x})"));
            }
            return Ok(());
        }
        let native = native(self.root(), path);
        // A file nobody else may read cannot be compared: what it holds is
        // not known, which is neither "as captured" nor "changed since".
        if let Err(error) = std::fs::File::open(&native) {
            if error.raw_os_error() == Some(SHARING_VIOLATION) {
                self.refuse(path, error);
                return Ok(());
            }
        }
        let captured = match was {
            Some((directory, was)) if !was.directory => {
                self.stamps.vouches(&native, path, item)
                    || self.holds(
                        path,
                        item,
                        self.was.as_ref(),
                        &join(directory, &was.name),
                        was,
                    )?
            }
            _ => false,
        };
        if !captured {
            // Changed after the capture read it, or new since. When it
            // already holds the new bytes there is nothing to tell.
            let there = match now {
                Some((directory, now)) => self.holds(
                    path,
                    item,
                    self.now.as_ref(),
                    &join(directory, &now.name),
                    now,
                )?,
                None => false,
            };
            if !there {
                self.stamps.forget(path);
                self.done.kept.push(path.to_owned());
            }
            return Ok(());
        }
        match now {
            Some((directory, now)) => {
                match write(
                    &native,
                    &self.scratch(),
                    self.now.as_ref(),
                    &join(directory, &now.name),
                    now,
                    self.sessions.as_ref(),
                ) {
                    Ok(stored) => {
                        self.note(path, stored);
                        self.done.replaced += 1;
                    }
                    Err(unwritten) if unwritten.touched => {
                        // Part of the new bytes may be in the file. What it
                        // held is known, so it is put back: the next
                        // Commit must not take the part for a change.
                        let restored = match was {
                            Some((directory, was)) => {
                                fill(&native, self.was.as_ref(), &join(directory, &was.name), was)
                                    .is_ok()
                            }
                            None => false,
                        };
                        let state = if restored {
                            "it holds the old bytes again"
                        } else {
                            "it may hold a part of the new bytes"
                        };
                        self.refuse(path, format!("{}; {state}", unwritten.error));
                    }
                    Err(unwritten) => self.refuse(path, unwritten.error),
                }
            }
            None => {
                self.stamps.forget(path);
                let removed = instance
                    .delete_item(&name, EVERY)
                    .is_ok_and(|changed| changed)
                    || read_only(&native, false)
                        .and_then(|()| std::fs::remove_file(&native))
                        .is_ok();
                if removed {
                    self.done.replaced += 1;
                } else {
                    self.refuse(path, "the file could not be removed");
                }
            }
        }
        // A directory took the file's place in the new tree.
        if let (false, Some((listed, entry))) = (projected, self.entry_at(path)) {
            if entry.directory && !native.exists() {
                self.arrive(path, &listed, &entry);
            }
        }
        Ok(())
    }

    /// The target's entry at `path`, with its listed path.
    fn entry_at(&self, path: &str) -> Option<(String, Entry)> {
        match self.target.resolve(path) {
            Ok(Some(Found::Entry { path, entry })) => Some((path, entry)),
            _ => None,
        }
    }

    /// Whether ProjFS lists the base's names in the directory at `path`:
    /// it does in the root and in a directory it made itself, and in no
    /// other.
    fn projected(&self, path: &str) -> Result<bool, WorkspaceError> {
        if path.is_empty() {
            return Ok(true);
        }
        self.local
            .state(path)
            .map(|state| state.is_some_and(|bits| bits & sys::state::PLACEHOLDER != 0))
            .map_err(|error| WorkspaceError::Failed(format!("baseline update: {error}")))
    }
}

/// Brings every item on disk from the tree `shown`, which the directory
/// showed when the capture read it, to `target`. `stamps` is the record of
/// committed files with what this capture read already in it; it is kept
/// true to what is written and forgets what could not be vouched for.
pub(super) fn follow(
    workspace: &Workspace,
    shown: &Tree,
    target: &Tree,
    stamps: &mut Ledger,
    behind: &Unfollowed,
) -> Result<Rebased, WorkspaceError> {
    let projection = &workspace.projection;
    let wrap = |e: &dyn std::fmt::Display| WorkspaceError::Failed(format!("baseline update: {e}"));
    let _local = projection.provider().local_only();
    let mut follow = Follow {
        workspace,
        target,
        was: shown.source(),
        now: target.source(),
        local: Local::new(projection.root()),
        stamps,
        sessions: Sessions::open(projection.root()).ok(),
        done: Rebased::default(),
    };
    let mut emptied = Vec::new();
    let mut walk = vec![String::new()];
    while let Some(at) = walk.pop() {
        let items = follow
            .local
            .list(&at)
            .map_err(|e| wrap(&e))?
            .unwrap_or_default();
        let was = directory(shown, &at).map_err(|e| wrap(&e))?;
        let now = directory(target, &at).map_err(|e| wrap(&e))?;
        let projected = follow.projected(&at)?;
        for item in &items {
            if item.link() {
                continue;
            }
            let path = join(&at, &item.name);
            let find = |held: &Option<(String, Arc<Directory>)>| {
                held.as_ref().and_then(|(directory, listed)| {
                    listed
                        .find(&item.name)
                        .map(|entry| (directory.clone(), entry.clone()))
                })
            };
            let (before, after) = (find(&was), find(&now));
            if item.directory {
                let held = |side: &Option<(String, Entry)>| {
                    side.as_ref().is_some_and(|(_, entry)| entry.directory)
                };
                if held(&before) && !held(&after) {
                    emptied.push(path.clone());
                }
                walk.push(path);
                continue;
            }
            if before.as_ref().map(|(_, entry)| entry) == after.as_ref().map(|(_, entry)| entry) {
                continue;
            }
            // Still showing an older tree than `shown`: an earlier merge
            // could not bring it over. It is retried from the tree it
            // shows, not judged against this one.
            if behind.holds(&path) {
                continue;
            }
            follow.file(
                &path,
                item,
                before
                    .as_ref()
                    .map(|(directory, entry)| (directory.as_str(), entry)),
                after
                    .as_ref()
                    .map(|(directory, entry)| (directory.as_str(), entry)),
                projected,
            )?;
        }
        // Names the merge added here that the disk does not hold.
        let Some((listed, held)) = &now else {
            continue;
        };
        let on_disk = Names::new(items.iter().map(|item| &item.name));
        for added in &held.items {
            let name = &added.entry.name;
            if on_disk.has(&added.wide)
                || was
                    .as_ref()
                    .is_some_and(|(_, shown)| shown.find(name).is_some())
            {
                continue;
            }
            let path = join(&at, name);
            // A tombstone of a name this Workspace once deleted would hide
            // it: it is cleared. There is none in most cases.
            let _ = projection
                .instance()
                .delete_item(&path.replace('/', "\\"), sys::ALLOW_TOMBSTONE);
            if !projected {
                follow.arrive(&path, &join(listed, name), &added.entry);
            }
        }
    }
    // Deepest first: a directory goes once what was below it is gone.
    emptied.sort();
    for path in emptied.into_iter().rev() {
        let native = native(projection.root(), &path);
        let removed = projection
            .instance()
            .delete_item(&path.replace('/', "\\"), EVERY)
            .is_ok_and(|changed| changed)
            || std::fs::remove_dir(&native).is_ok();
        if !removed {
            continue;
        }
        follow.done.removed += 1;
        // A file took the directory's place in the new tree.
        let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        if let (false, Some((listed, entry))) = (follow.projected(parent)?, follow.entry_at(&path))
        {
            follow.arrive(&path, &listed, &entry);
        }
    }
    Ok(follow.done)
}

/// Tries again to bring `paths` from the tree `shown`, which each of them
/// still shows on disk, to `target`. A path that is brought over, or that
/// holds the target's bytes already, is neither refused nor kept in the
/// answer; a path that still cannot be changed is refused again; a path
/// that holds something else is kept.
pub(super) fn retry(
    workspace: &Workspace,
    shown: &Tree,
    target: &Tree,
    paths: &[String],
    stamps: &mut Ledger,
) -> Result<Rebased, WorkspaceError> {
    let projection = &workspace.projection;
    let wrap = |e: &dyn std::fmt::Display| WorkspaceError::Failed(format!("baseline update: {e}"));
    let _local = projection.provider().local_only();
    let mut follow = Follow {
        workspace,
        target,
        was: shown.source(),
        now: target.source(),
        local: Local::new(projection.root()),
        stamps,
        sessions: Sessions::open(projection.root()).ok(),
        done: Rebased::default(),
    };
    for path in paths {
        let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let name = path.rsplit('/').next().unwrap_or(path);
        let side = |tree: &Tree| -> Result<Option<(String, Entry)>, WorkspaceError> {
            Ok(directory(tree, parent)
                .map_err(|e| wrap(&e))?
                .and_then(|(listed, held)| held.find(name).map(|entry| (listed, entry.clone()))))
        };
        let (before, after) = (side(shown)?, side(target)?);
        let projected = follow.projected(parent)?;
        let Some((spelled, item)) = follow.local.find(path).map_err(|e| wrap(&e))? else {
            // Nothing on disk: the name is projected from the tree served
            // now, unless ProjFS projects nothing into its directory.
            if let (false, Some((listed, entry))) = (projected, &after) {
                follow.arrive(path, &join(listed, &entry.name), entry);
            }
            continue;
        };
        if item.directory || item.link() {
            continue;
        }
        follow.file(
            &spelled,
            &item,
            before
                .as_ref()
                .map(|(listed, entry)| (listed.as_str(), entry)),
            after
                .as_ref()
                .map(|(listed, entry)| (listed.as_str(), entry)),
            projected,
        )?;
    }
    Ok(follow.done)
}
