//! What changed in a mount: the paths the change journal marks, each
//! compared between the local disk and the projected base. Work follows the
//! number of changed items (and the bytes of rewritten files of unchanged
//! length). The tree is walked only when the journal could not answer.
use super::journal::{Recorded, Rename};
use super::local::{native, Local, LocalItem};
use super::watch::Dirty;
use crate::provider::tree::{join, Found};
use crate::provider::Provider;
use crate::source::{attribute, Entry, Source, SourceError};
use crate::sys;
use std::collections::{BTreeMap, BTreeSet};
use std::os::windows::fs::FileExt;

/// How one path differs from the base.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Kind {
    Added,
    Modified,
    Deleted,
    /// The entry the base holds at `from` is now at this path, unchanged.
    Renamed {
        from: String,
    },
}

/// One changed path, `/`-separated, in its on-disk spelling (or the
/// base's, for a deleted path).
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Change {
    pub path: String,
    pub kind: Kind,
    pub directory: bool,
}

/// Why a status could not be computed.
#[derive(Debug)]
pub enum StatusError {
    Local(std::io::Error),
    Source(SourceError),
    ProjFs(sys::Error),
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(error) => write!(f, "local disk: {error}"),
            Self::Source(error) => write!(f, "base: {error}"),
            Self::ProjFs(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for StatusError {}
impl From<std::io::Error> for StatusError {
    fn from(error: std::io::Error) -> Self {
        Self::Local(error)
    }
}
impl From<SourceError> for StatusError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}
impl From<sys::Error> for StatusError {
    fn from(error: sys::Error) -> Self {
        Self::ProjFs(error)
    }
}

fn folded(path: &str) -> String {
    path.to_lowercase()
}

/// Where the item now at `path` was before the recorded renames, and
/// whether `path` itself (not a directory above it) was renamed.
pub fn origin(path: &str, renames: &[Rename]) -> Option<(String, bool)> {
    let (mut at, mut exact, mut moved) = (path.to_owned(), true, false);
    for rename in renames.iter().rev() {
        let (current, to) = (folded(&at), folded(&rename.to));
        if current == to {
            at = rename.from.clone();
            moved = true;
        } else if current.starts_with(&format!("{to}/")) {
            at = format!("{}{}", rename.from, &at[rename.to.len()..]);
            exact = false;
            moved = true;
        }
    }
    moved.then_some((at, exact))
}

/// Bytes compared per read.
const WINDOW: usize = 1 << 20;
const SHARING_VIOLATION: i32 = 32;

/// Whether the local file at `path` holds other bytes than `base`.
pub fn bytes_differ(
    local: &Local,
    path: &str,
    item: &LocalItem,
    source: &dyn Source,
    base_path: &str,
    base: &Entry,
) -> Result<bool, StatusError> {
    if item.size != base.size {
        return Ok(true);
    }
    let file = match std::fs::File::open(native(local.root(), path)) {
        Ok(file) => file,
        // Another process holds it exclusively: it cannot be compared, and
        // a full file nobody can read is reported as changed.
        Err(error) if error.raw_os_error() == Some(SHARING_VIOLATION) => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    let (mut here, mut there) = (vec![0u8; WINDOW], vec![0u8; WINDOW]);
    let mut offset = 0u64;
    while offset < base.size {
        let step = ((base.size - offset) as usize).min(WINDOW);
        let mut filled = 0;
        while filled < step {
            let read = file.seek_read(&mut here[filled..step], offset + filled as u64)?;
            if read == 0 {
                return Ok(true);
            }
            filled += read;
        }
        source.read(base_path, &base.content, offset, &mut there[..step])?;
        if here[..step] != there[..step] {
            return Ok(true);
        }
        offset += step as u64;
    }
    Ok(false)
}

/// Whether the local file differs from the base file: other bytes, or
/// another read-only attribute. A placeholder, hydrated or not, still holds
/// the base's bytes by construction and is never read.
fn file_differs(
    local: &Local,
    path: &str,
    item: &LocalItem,
    source: &dyn Source,
    base_path: &str,
    base: &Entry,
    committed: bool,
) -> Result<bool, StatusError> {
    if (item.attributes ^ base.attributes) & attribute::READ_ONLY != 0 {
        return Ok(true);
    }
    let full = local
        .state(path)?
        .is_some_and(|bits| bits & sys::state::FULL != 0);
    if !full || committed {
        return Ok(false);
    }
    bytes_differ(local, path, item, source, base_path, base)
}

/// Decides whether a full file on disk is exactly what the last Commit
/// stored at its path, from the Workspace's record of committed files.
pub type Known<'a> = &'a dyn Fn(&str, &LocalItem) -> bool;

/// What a status found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    /// The changes against the base, sorted by path.
    pub changes: Vec<Change>,
    /// Junctions and symbolic links on disk. They are never followed and
    /// never committed.
    pub links: Vec<String>,
}

struct Classifier<'a> {
    provider: &'a Provider,
    source: std::sync::Arc<dyn Source>,
    local: Local,
    renames: &'a [Rename],
    dirty: &'a Dirty,
    known: Known<'a>,
    links: Vec<String>,
    seen: BTreeSet<String>,
    changes: Vec<Change>,
    deleted: BTreeMap<String, bool>,
    /// Base paths a rename accounts for; they are not reported deleted.
    moved: BTreeSet<String>,
}

impl Classifier<'_> {
    fn base(&self, typed: &str) -> Result<Option<(String, Entry)>, StatusError> {
        Ok(match self.provider.tree.resolve(typed)? {
            Some(Found::Entry { path, entry }) => Some((path, entry)),
            _ => None,
        })
    }

    /// Whether the full file at `path` is what the last Commit stored. A
    /// file still open for writing is never taken on its size and time.
    fn committed(&self, path: &str, item: &LocalItem) -> bool {
        !self.dirty.live.contains(path) && (self.known)(path, item)
    }

    /// Compares the names a directory shows now with the names its base
    /// directory holds.
    fn members(&mut self, directory: &str, listed: &[String]) -> Result<(), StatusError> {
        let renamed = origin(directory, self.renames);
        let from = renamed
            .as_ref()
            .map_or(directory, |(from, _)| from.as_str());
        let base = match self.provider.tree.resolve(from)? {
            Some(found) if found.is_directory() => {
                Some(self.provider.tree.directory(found.path())?)
            }
            _ => None,
        };
        if let Some(base) = &base {
            let base_path = self
                .provider
                .tree
                .resolve(from)?
                .map(|found| found.path().to_owned())
                .unwrap_or_default();
            for item in &base.items {
                let shown = listed
                    .iter()
                    .any(|name| sys::compare(&sys::wide(name), &item.wide).is_eq());
                if shown {
                    continue;
                }
                if renamed.is_some() {
                    // Below a renamed directory the old path names nothing
                    // a reader can find; the loss is told where it is now.
                    self.changes.push(Change {
                        path: join(directory, &item.entry.name),
                        kind: Kind::Deleted,
                        directory: item.entry.directory,
                    });
                } else {
                    self.deleted
                        .insert(join(&base_path, &item.entry.name), item.entry.directory);
                }
            }
        }
        for name in listed {
            // New to the base, or held by it under another spelling.
            let held = base.as_ref().and_then(|base| base.find(name));
            if held.is_none_or(|entry| entry.name != *name) {
                self.classify(&join(directory, name))?;
            }
        }
        Ok(())
    }

    fn add(&mut self, path: &str, item: &LocalItem) -> Result<(), StatusError> {
        self.changes.push(Change {
            path: path.to_owned(),
            kind: Kind::Added,
            directory: item.directory,
        });
        if item.directory {
            // A directory new to the base is reported with all it holds: its
            // children may have arrived with it, unreported one by one.
            for child in self.local.list(path)?.unwrap_or_default() {
                let child_path = join(path, &child.name);
                if !self.seen.insert(folded(&child_path)) {
                    continue;
                }
                if child.link() {
                    self.links.push(child_path);
                } else {
                    self.add(&child_path, &child)?;
                }
            }
        }
        Ok(())
    }

    /// Classifies an on-disk item the base holds nothing for at its path.
    fn unmatched(&mut self, path: &str, item: &LocalItem) -> Result<(), StatusError> {
        let Some((from, exact)) = origin(path, self.renames) else {
            return self.add(path, item);
        };
        let Some((base_path, base)) = self.base(&from)? else {
            return self.add(path, item);
        };
        // The old name must be gone: otherwise this is a copy, not a move.
        if base.directory != item.directory || self.local.find(&base_path)?.is_some() {
            return self.add(path, item);
        }
        let same = item.directory
            || !file_differs(
                &self.local,
                path,
                item,
                self.source.as_ref(),
                &base_path,
                &base,
                false,
            )?;
        match (exact, same) {
            (true, true) => {
                self.moved.insert(base_path.clone());
                self.changes.push(Change {
                    path: path.to_owned(),
                    kind: Kind::Renamed { from: base_path },
                    directory: item.directory,
                });
            }
            // Below a renamed directory: only a difference is a change.
            (false, true) => {}
            (false, false) => self.changes.push(Change {
                path: path.to_owned(),
                kind: Kind::Modified,
                directory: false,
            }),
            (true, false) => self.add(path, item)?,
        }
        Ok(())
    }

    fn classify(&mut self, typed: &str) -> Result<(), StatusError> {
        let Some((path, item)) = self.local.find(typed)? else {
            if let Some((base_path, base)) = self.base(typed)? {
                self.deleted.insert(base_path, base.directory);
            }
            return Ok(());
        };
        if !self.seen.insert(folded(&path)) {
            return Ok(());
        }
        if item.link() {
            // Never followed: named, and what the base held there is gone.
            self.links.push(path.clone());
            if let Some((base_path, base)) = self.base(&path)? {
                self.deleted.insert(base_path, base.directory);
            }
            return Ok(());
        }
        match self.base(&path)? {
            None => self.unmatched(&path, &item),
            Some((base_path, base)) if base.directory != item.directory => {
                self.deleted.insert(base_path, base.directory);
                self.add(&path, &item)
            }
            Some((base_path, base)) => {
                // Found under another spelling: only the name's case changed.
                let spelled = |path: &str| path.rsplit('/').next().map(str::to_owned);
                if spelled(&base_path) != spelled(&path) {
                    self.changes.push(Change {
                        path: path.clone(),
                        kind: Kind::Renamed {
                            from: base_path.clone(),
                        },
                        directory: item.directory,
                    });
                }
                if !item.directory
                    && file_differs(
                        &self.local,
                        &path,
                        &item,
                        self.source.as_ref(),
                        &base_path,
                        &base,
                        self.committed(&path, &item),
                    )?
                {
                    self.changes.push(Change {
                        path,
                        kind: Kind::Modified,
                        directory: false,
                    });
                }
                Ok(())
            }
        }
    }
}

/// The mount's changes against its base. `recorded` holds the renames
/// ProjFS reported, which tell a move from a deletion and an addition;
/// `dirty` is what the change journal marks. The caller holds the
/// provider's `LocalOnly` guard through `local`'s lifetime.
pub fn changes(
    provider: &Provider,
    local: Local,
    recorded: &Recorded,
    dirty: &Dirty,
    known: Known<'_>,
) -> Result<Report, StatusError> {
    let mut classifier = Classifier {
        provider,
        source: provider.tree.source(),
        local,
        renames: &recorded.renames,
        dirty,
        known,
        links: Vec::new(),
        seen: BTreeSet::new(),
        changes: Vec::new(),
        deleted: BTreeMap::new(),
        moved: BTreeSet::new(),
    };
    // A renamed item is looked at first: it accounts for its old name.
    for rename in &recorded.renames {
        classifier.classify(&rename.to)?;
    }
    for (directory, listed) in &dirty.listings {
        classifier.members(directory, listed)?;
    }
    for typed in &dirty.paths {
        classifier.classify(typed)?;
    }
    let Classifier {
        mut changes,
        deleted,
        moved,
        mut links,
        ..
    } = classifier;
    // A deleted or renamed directory accounts for everything below it.
    let covered: Vec<String> = deleted
        .iter()
        .filter(|(_, directory)| **directory)
        .map(|(path, _)| path.clone())
        .chain(moved.iter().cloned())
        .collect();
    for (path, directory) in deleted {
        let below = covered
            .iter()
            .any(|above| path.len() > above.len() && path.starts_with(&format!("{above}/")));
        if !below && !moved.contains(&path) {
            changes.push(Change {
                path,
                kind: Kind::Deleted,
                directory,
            });
        }
    }
    changes.sort();
    changes.dedup();
    links.sort();
    Ok(Report { changes, links })
}

/// What a mount holds on the local disk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    /// Directories on disk.
    pub directories: u64,
    /// Files on disk that hold none of their content.
    pub placeholders: u64,
    /// Files on disk that hold the base's content.
    pub hydrated: u64,
    /// Files on disk that are the Workspace's own: created or opened for
    /// writing.
    pub full: u64,
    /// Content bytes of the hydrated and full files (their lengths; the
    /// volume rounds each up to a cluster).
    pub bytes: u64,
}

/// Counts what is on the local disk below the root. Tombstones cannot be
/// seen while a provider runs and are not counted. The caller holds the
/// provider's `LocalOnly` guard.
pub fn usage(local: &mut Local) -> Result<Usage, StatusError> {
    let mut usage = Usage::default();
    let mut pending = vec![String::new()];
    while let Some(directory) = pending.pop() {
        for item in local.list(&directory)?.unwrap_or_default() {
            let path = join(&directory, &item.name);
            if item.directory {
                usage.directories += 1;
                pending.push(path);
                continue;
            }
            let state = local.state(&path)?.unwrap_or(sys::state::FULL);
            if state & sys::state::FULL != 0 {
                usage.full += 1;
                usage.bytes += item.size;
            } else if state & sys::state::HYDRATED != 0 {
                usage.hydrated += 1;
                usage.bytes += item.size;
            } else {
                usage.placeholders += 1;
            }
        }
    }
    Ok(usage)
}
