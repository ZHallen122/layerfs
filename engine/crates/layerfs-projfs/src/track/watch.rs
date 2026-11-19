//! Which items below the root changed, from the volume's own change journal.
//!
//! NTFS writes a journal record when a change to a file first happens, not
//! when the handle that made it closes, and records what it holds below the
//! ProjFS filter. So this is where a status or Commit learns what to look
//! at. It does not depend on a ProjFS notification arriving, and a write
//! through a handle that is still open is in it.
//!
//! The records carry no names (an unprivileged read omits them), only
//! identities. A changed file is found again by its identity; a directory
//! in which a name appeared or disappeared is listed and compared. Records
//! are told apart by the attributes NTFS holds:
//!
//! - a reparse point that is hidden and system is a tombstone, ProjFS's
//!   marker for a deleted or renamed-away name: its directory lost a name;
//! - any other reparse point is a placeholder, whose bytes are the base's
//!   by construction: only a change of its read-only bit matters;
//! - everything else is an ordinary file or directory.
//!
//! A file that changed through a handle not yet closed stays suspect at
//! every Commit until the journal shows the close: later writes through the
//! same handle add no record.
use crate::ntfs::{attribute, reason, JournalError, Record, Volume};
use crate::track::local::native;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

/// Where a reader of a journal stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Position {
    pub journal: u64,
    pub usn: i64,
}

/// What changed below the root, as identities on the volume.
#[derive(Clone, Debug, Default)]
pub struct Pending {
    /// Ordinary files whose bytes, attributes or name changed.
    pub files: BTreeSet<u64>,
    /// Directories in which a name appeared or disappeared.
    pub members: BTreeSet<u64>,
    /// Placeholders whose read-only attribute changed.
    pub attributes: BTreeSet<u64>,
    /// Directories that arrived whole, with content the journal did not
    /// attribute to the root.
    pub adopted: BTreeSet<u64>,
    /// Files changed through a handle the journal has not shown closed.
    pub open: BTreeSet<u64>,
    /// The journal could not answer for some of the time: everything on
    /// disk is suspect.
    pub everything: bool,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        !self.everything
            && self.files.is_empty()
            && self.members.is_empty()
            && self.attributes.is_empty()
            && self.adopted.is_empty()
    }

    fn absorb(&mut self, other: Self) {
        self.files.extend(other.files);
        self.members.extend(other.members);
        self.attributes.extend(other.attributes);
        self.adopted.extend(other.adopted);
        self.open.extend(other.open);
        self.everything |= other.everything;
    }
}

/// The changed items as paths: what a status or capture compares.
#[derive(Clone, Debug, Default)]
pub struct Dirty {
    /// Paths to compare, relative to the root and `/`-separated, as the
    /// disk spells them now.
    pub paths: BTreeSet<String>,
    /// Directories whose names changed, each with the names a listing of it
    /// shows now. A name the base holds and the listing lacks was deleted
    /// or renamed away; a name the listing has and the base lacks is new.
    pub listings: BTreeMap<String, Vec<String>>,
    /// The paths among `paths` whose file is still open for writing: its
    /// size and time say nothing about its bytes.
    pub live: BTreeSet<String>,
}

struct State {
    journal: u64,
    position: i64,
    /// Every directory on disk below the root, the root included.
    directories: HashSet<u64>,
    /// The read-only bit each placeholder was created with.
    read_only: HashMap<u64, bool>,
    /// Files changed through a handle the journal has not shown closed.
    open: HashSet<u64>,
    pending: Pending,
}

/// The tracker of one mount.
pub struct Watch {
    volume: Volume,
    /// The root as the volume spells it, with a trailing separator.
    prefix: String,
    state: Mutex<State>,
    stopped: AtomicBool,
}

fn ingest(state: &mut State, record: &Record) {
    let inside = state.directories.contains(&record.parent);
    let known = state.directories.contains(&record.id);
    if !inside && !known {
        return;
    }
    let (id, reasons, attributes) = (record.id, record.reason, record.attributes);
    let reparse = attributes & attribute::REPARSE_POINT != 0;
    let marker = attribute::HIDDEN | attribute::SYSTEM;
    if reparse && attributes & marker == marker {
        if inside {
            state.pending.members.insert(record.parent);
        }
        return;
    }
    if attributes & attribute::DIRECTORY != 0 {
        if reasons & reason::FILE_DELETE != 0 {
            state.directories.remove(&id);
        } else if inside && !known {
            state.directories.insert(id);
            if !reparse && reasons & reason::RENAME_NEW_NAME != 0 {
                state.pending.adopted.insert(id);
            }
        }
        // A placeholder directory appears when a process first looks into
        // it; the base already holds its name.
        if inside && !reparse && reasons & reason::NAME != 0 {
            state.pending.members.insert(record.parent);
        }
        return;
    }
    if reparse {
        let read_only = attributes & attribute::READ_ONLY != 0;
        if reasons & reason::FILE_CREATE != 0 {
            state.read_only.insert(id, read_only);
        } else if reasons & reason::BASIC_INFO_CHANGE != 0
            && state.read_only.get(&id) != Some(&read_only)
        {
            state.pending.attributes.insert(id);
        }
        let renamed = reason::RENAME_OLD_NAME | reason::RENAME_NEW_NAME | reason::HARD_LINK_CHANGE;
        if inside && reasons & renamed != 0 {
            state.pending.members.insert(record.parent);
        }
        return;
    }
    if inside && reasons & reason::NAME != 0 {
        state.pending.members.insert(record.parent);
    }
    if reasons & reason::FILE_DELETE != 0 {
        state.open.remove(&id);
        return;
    }
    let content = reason::DATA | reason::NAMED_DATA | reason::STREAM_CHANGE;
    let relevant = content
        | reason::BASIC_INFO_CHANGE
        | reason::NAME
        | reason::REPARSE_POINT_CHANGE
        | reason::EA_CHANGE;
    if reasons & relevant != 0 {
        state.pending.files.insert(id);
    }
    if reasons & reason::CLOSE != 0 {
        state.open.remove(&id);
    } else if reasons & content != 0 {
        state.open.insert(id);
    }
}

/// How often the journal is read while nothing asks.
const TAIL: std::time::Duration = std::time::Duration::from_millis(200);

impl Watch {
    /// Starts tracking the mount at `root`. `from` is the position a
    /// previous provider settled at, with `directories` the identities of
    /// the directories on disk below the root; a fresh mount passes `None`
    /// and nothing. A thread keeps reading the journal so that a burst of
    /// activity on the volume cannot push unread records out of it.
    pub fn start(
        root: &Path,
        from: Option<Position>,
        directories: impl IntoIterator<Item = u64>,
    ) -> Result<Arc<Self>, JournalError> {
        let volume = Volume::open(root).map_err(JournalError::Io)?;
        let journal = volume.journal()?;
        let own = volume.directory().map_err(JournalError::Io)?.index;
        let spelled = volume.path(own).map_err(JournalError::Io)?.ok_or_else(|| {
            JournalError::Lost("the root cannot be opened by identity".to_owned())
        })?;
        let mut pending = Pending::default();
        let position = match from {
            Some(position) if position.journal == journal.id && position.usn >= journal.first => {
                position.usn
            }
            Some(_) => {
                pending.everything = true;
                journal.next
            }
            None => journal.next,
        };
        let mut known: HashSet<u64> = directories.into_iter().collect();
        known.insert(own);
        let watch = Arc::new(Self {
            volume,
            prefix: format!("{}\\", spelled.to_string_lossy().trim_end_matches('\\')),
            state: Mutex::new(State {
                journal: journal.id,
                position,
                directories: known,
                read_only: HashMap::new(),
                open: HashSet::new(),
                pending,
            }),
            stopped: AtomicBool::new(false),
        });
        let weak: Weak<Self> = Arc::downgrade(&watch);
        // Without the thread nothing is lost: every question reads first.
        let _ = std::thread::Builder::new()
            .name("layerfs-projfs-journal".to_owned())
            .spawn(move || loop {
                std::thread::sleep(TAIL);
                match weak.upgrade() {
                    Some(watch) if !watch.stopped.load(Ordering::Acquire) => watch.advance(),
                    _ => return,
                }
            });
        Ok(watch)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Ends the reading thread.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    /// Reads the journal to its end.
    pub fn advance(&self) {
        let mut guard = self.lock();
        let state = &mut *guard;
        let (journal, from) = (state.journal, state.position);
        let read = self
            .volume
            .read(journal, from, &mut |record| ingest(state, record));
        match read {
            Ok(next) => state.position = next,
            Err(_) => {
                // What happened since `from` is unknown. Tracking starts
                // again at the journal's present end, and everything on
                // disk is compared once.
                state.pending.everything = true;
                if let Ok(journal) = self.volume.journal() {
                    state.journal = journal.id;
                    state.position = journal.next;
                }
            }
        }
    }

    /// What is pending now, read to the journal's end, and the position
    /// everything pending lies before.
    pub fn pending(&self) -> (Pending, Position) {
        self.advance();
        let state = self.lock();
        let mut pending = state.pending.clone();
        pending.open = state.open.iter().copied().collect();
        pending.files.extend(state.open.iter().copied());
        (
            pending,
            Position {
                journal: state.journal,
                usn: state.position,
            },
        )
    }

    /// Takes what is pending, leaving the tracker empty for what comes
    /// after. The caller gives it back with [`Self::restore`] unless a
    /// Commit published it.
    pub fn take(&self) -> (Pending, Position) {
        self.advance();
        let mut state = self.lock();
        let mut pending = std::mem::take(&mut state.pending);
        pending.open = state.open.iter().copied().collect();
        pending.files.extend(state.open.iter().copied());
        (
            pending,
            Position {
                journal: state.journal,
                usn: state.position,
            },
        )
    }

    /// Puts back what [`Self::take`] took: nothing was published.
    pub fn restore(&self, pending: Pending) {
        self.lock().pending.absorb(pending);
    }

    /// Registers a directory that is on disk below the root.
    pub fn know(&self, directory: u64) {
        self.lock().directories.insert(directory);
    }

    /// The path of the item with this identity, relative to the root and
    /// `/`-separated; `None` when it is gone or no longer below the root.
    pub fn resolve(&self, id: u64) -> std::io::Result<Option<String>> {
        let Some(path) = self.volume.path(id)? else {
            return Ok(None);
        };
        let text = format!("{}\\", path.to_string_lossy());
        Ok(text
            .strip_prefix(&self.prefix)
            .map(|below| below.trim_end_matches('\\').replace('\\', "/")))
    }

    /// Turns pending identities into the paths and listings to compare.
    /// The listings are what any process sees, so the caller must not hold
    /// the provider to its local-only view.
    pub fn examine(&self, root: &Path, pending: &Pending) -> std::io::Result<Dirty> {
        let mut dirty = Dirty::default();
        for id in pending.files.iter().chain(&pending.attributes) {
            if let Some(path) = self.resolve(*id)?.filter(|path| !path.is_empty()) {
                if pending.open.contains(id) {
                    dirty.live.insert(path.clone());
                }
                dirty.paths.insert(path);
            }
        }
        for id in &pending.adopted {
            if let Some(path) = self.resolve(*id)? {
                self.adopt(&native(root, &path))?;
            }
        }
        for id in &pending.members {
            let Some(path) = self.resolve(*id)? else {
                continue;
            };
            if let Some(names) = names(&native(root, &path))? {
                dirty.listings.insert(path, names);
            }
        }
        Ok(dirty)
    }

    /// Registers every directory below `directory`. Links are not followed.
    fn adopt(&self, directory: &Path) -> std::io::Result<()> {
        let mut pending = vec![directory.to_owned()];
        while let Some(at) = pending.pop() {
            let Ok(read) = std::fs::read_dir(&at) else {
                continue;
            };
            for entry in read {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    if let Ok(volume) = Volume::open(&entry.path()) {
                        self.know(volume.directory()?.index);
                    }
                    pending.push(entry.path());
                }
            }
        }
        Ok(())
    }
}

/// The names a listing of `directory` shows; `None` when it is not there.
pub fn names(directory: &Path) -> std::io::Result<Option<Vec<String>>> {
    let read = match std::fs::read_dir(directory) {
        Ok(read) => read,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in read {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    Ok(Some(names))
}

/// The identities of the directories on disk below `root`, read while no
/// provider serves it: the file system then lists exactly what is on disk.
pub fn directories(root: &Path) -> std::io::Result<Vec<u64>> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(at) = pending.pop() {
        for entry in std::fs::read_dir(&at)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Ok(volume) = Volume::open(&entry.path()) {
                    found.push(volume.directory()?.index);
                }
                pending.push(entry.path());
            }
        }
    }
    Ok(found)
}
