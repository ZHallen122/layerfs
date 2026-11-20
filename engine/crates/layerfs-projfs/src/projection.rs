//! One mounted projection of a source into a local directory: start, status,
//! unmount, discard. The Workspace entry point composes this with the Store.
use crate::ntfs::Volume;
use crate::provider::tree::join;
use crate::provider::Provider;
use crate::source::{attribute, Source};
use crate::sys;
use crate::track::local::{native, verbatim, Local};
use crate::track::status::{self, Change, Known, Report, StatusError};
use crate::track::watch::{self, Dirty, Kept, Pending, Position, Watch};
use crate::track::Journal;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Why a projection could not be mounted or ended.
#[derive(Debug)]
pub enum MountError {
    /// ProjFS is not enabled on this machine; the text says how to enable it.
    Unavailable(sys::Unavailable),
    /// A new mount needs an empty or absent directory.
    NotEmpty(PathBuf),
    /// The state directory is the root or inside it.
    StateInsideRoot,
    /// The state directory holds no mount to resume.
    NoMount(PathBuf),
    Io {
        what: &'static str,
        path: PathBuf,
        error: std::io::Error,
    },
    ProjFs(sys::Error),
    /// The volume's change journal cannot be read, so changes cannot be
    /// tracked.
    Journal(String),
    /// The state directory belongs to another directory than the one given.
    WrongRoot(PathBuf),
}

impl std::fmt::Display for MountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => write!(f, "Unsupported: {reason}"),
            Self::NotEmpty(path) => write!(f, "NotEmpty: {} is not empty", path.display()),
            Self::StateInsideRoot => {
                f.write_str("InvalidInput: the state directory must be outside the mount")
            }
            Self::NoMount(path) => write!(f, "NotMounted: no mount state at {}", path.display()),
            Self::Io { what, path, error } => {
                write!(f, "Failed: {what} {}: {error}", path.display())
            }
            Self::ProjFs(error) => write!(f, "Failed: {error}"),
            Self::Journal(text) => write!(f, "Unsupported: {text}"),
            Self::WrongRoot(path) => write!(
                f,
                "InvalidInput: the state directory is not the one of {}",
                path.display()
            ),
        }
    }
}
impl std::error::Error for MountError {}

impl From<sys::Error> for MountError {
    fn from(error: sys::Error) -> Self {
        match error {
            sys::Error::Unavailable(reason) => Self::Unavailable(reason),
            other => Self::ProjFs(other),
        }
    }
}

fn io<'a>(what: &'static str, path: &'a Path) -> impl FnOnce(std::io::Error) -> MountError + 'a {
    move |error| MountError::Io {
        what,
        path: path.to_owned(),
        error,
    }
}

fn empty(path: &Path) -> Result<bool, MountError> {
    match std::fs::read_dir(path) {
        Ok(mut entries) => Ok(entries.next().is_none()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(io("read", path)(error)),
    }
}

/// A fresh instance identity: unique on this machine, not a secret.
pub fn identity() -> u128 {
    let half = |salt: u64| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.write_u32(std::process::id());
        hasher.finish()
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos() as u64)
        .unwrap_or_default();
    (u128::from(half(nanos)) << 64) | u128::from(half(!nanos))
}

/// Removes a directory tree, read-only items included.
pub fn remove_tree(path: &Path) -> std::io::Result<()> {
    let path = verbatim(path);
    match std::fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => {
            clear_read_only(&path)?;
            std::fs::remove_dir_all(&path)
        }
    }
}

fn clear_read_only(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    let mut permissions = metadata.permissions();
    if permissions.readonly() {
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)?;
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            clear_read_only(&entry?.path())?;
        }
    }
    Ok(())
}

/// What a state directory records about its mount's place on the volume:
/// which directory it belongs to, the change journal position every
/// uncommitted change lies after, and the files that were open for writing
/// at that position. A later write through such a handle adds no journal
/// record, so the list is as much a part of "what may have changed" as the
/// position is.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Anchor {
    volume: u32,
    directory: u64,
    position: Position,
    open: Vec<u64>,
}

impl Anchor {
    fn read(state: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(state.join("anchor")).ok()?;
        let field = |name: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
        };
        Some(Self {
            volume: field("volume")?.parse().ok()?,
            directory: field("directory")?.parse().ok()?,
            position: Position {
                journal: field("journal")?.parse().ok()?,
                usn: field("usn")?.parse().ok()?,
            },
            open: field("open")
                .unwrap_or_default()
                .split(',')
                .filter(|id| !id.is_empty())
                .map(|id| id.parse().ok())
                .collect::<Option<_>>()?,
        })
    }

    fn write(&self, state: &Path) -> std::io::Result<()> {
        // Written beside and renamed in: a torn record would lose the
        // position every uncommitted change is found from.
        let (path, beside) = (state.join("anchor"), state.join("anchor.new"));
        let open: Vec<String> = self.open.iter().map(u64::to_string).collect();
        std::fs::write(
            &beside,
            format!(
                "volume={}\ndirectory={}\njournal={}\nusn={}\nopen={}\n",
                self.volume,
                self.directory,
                self.position.journal,
                self.position.usn,
                open.join(",")
            ),
        )?;
        std::fs::rename(beside, path)
    }
}

/// A source projected into a local directory by a running provider.
pub struct Projection {
    root: PathBuf,
    state: PathBuf,
    provider: Arc<Provider>,
    instance: Option<sys::Instance>,
    watch: Arc<Watch>,
    anchor: std::sync::Mutex<Anchor>,
}

impl Projection {
    /// Mounts `source` at the empty (or absent) directory `root`. `state`
    /// is a directory outside `root` that holds the mount's local records;
    /// it must be empty or absent too. `threads` is the provider's worker
    /// count (0 lets ProjFS choose).
    pub fn mount(
        root: &Path,
        state: &Path,
        source: Arc<dyn Source>,
        threads: u32,
    ) -> Result<Self, MountError> {
        sys::available().map_err(MountError::Unavailable)?;
        if !empty(root)? {
            return Err(MountError::NotEmpty(root.to_owned()));
        }
        if !empty(state)? {
            return Err(MountError::NotEmpty(state.to_owned()));
        }
        std::fs::create_dir_all(root).map_err(io("create", root))?;
        std::fs::create_dir_all(state).map_err(io("create", state))?;
        Self::outside(root, state)?;
        let instance = identity();
        sys::mark_root(root, instance)?;
        let record = state.join("instance");
        std::fs::write(&record, format!("{instance:032x}\n")).map_err(io("write", &record))?;
        // Tracking starts before anything can change: at the journal's end.
        let watch = Watch::start(root, Kept::default(), [])
            .map_err(|e| MountError::Journal(e.to_string()))?;
        let anchor = Self::anchor(root, watch.pending().1)?;
        anchor.write(state).map_err(io("write", state))?;
        Self::start(root, state, source, threads, watch, anchor)
    }

    fn anchor(root: &Path, position: Position) -> Result<Anchor, MountError> {
        let own = Volume::open(root)
            .and_then(|volume| volume.directory())
            .map_err(io("identify", root))?;
        Ok(Anchor {
            volume: own.volume,
            directory: own.index,
            position,
            open: Vec::new(),
        })
    }

    /// Serves again a root a provider stopped serving, by unmount or by
    /// dying. The items on disk stay as they are. What changed while no
    /// provider ran is read from the volume's change journal, from the
    /// position the state directory records; when that position is not one
    /// the journal can be read from (its records are overwritten, it lies
    /// after the journal's end, or the journal is another one), everything
    /// on disk is compared once. Returns how many changed items the
    /// journal already shows.
    pub fn resume(
        root: &Path,
        state: &Path,
        source: Arc<dyn Source>,
        threads: u32,
    ) -> Result<(Self, usize), MountError> {
        sys::available().map_err(MountError::Unavailable)?;
        if !state.join("instance").is_file() || !root.is_dir() {
            return Err(MountError::NoMount(state.to_owned()));
        }
        let anchor = Anchor::read(state).ok_or_else(|| MountError::NoMount(state.to_owned()))?;
        Self::belongs(root, &anchor)?;
        // No provider runs yet, so the listing is exactly what is on disk.
        let known = watch::directories(root).map_err(io("list", root))?;
        let kept = Kept {
            position: Some(anchor.position),
            open: anchor.open.clone(),
        };
        let watch =
            Watch::start(root, kept, known).map_err(|e| MountError::Journal(e.to_string()))?;
        let resumed = Self::start(root, state, source, threads, watch, anchor)?;
        let (pending, _) = resumed.watch.pending();
        let found = pending.files.len()
            + pending.members.len()
            + pending.attributes.len()
            + pending.adopted.len()
            + usize::from(pending.everything);
        Ok((resumed, found))
    }

    /// Refuses a state directory that records another directory.
    fn belongs(root: &Path, anchor: &Anchor) -> Result<(), MountError> {
        let own = Self::anchor(root, anchor.position)?;
        if (own.volume, own.directory) == (anchor.volume, anchor.directory) {
            Ok(())
        } else {
            Err(MountError::WrongRoot(root.to_owned()))
        }
    }

    fn outside(root: &Path, state: &Path) -> Result<(), MountError> {
        let root = std::fs::canonicalize(root).map_err(io("resolve", root))?;
        let state = std::fs::canonicalize(state).map_err(io("resolve", state))?;
        if state.starts_with(&root) {
            return Err(MountError::StateInsideRoot);
        }
        Ok(())
    }

    fn start(
        root: &Path,
        state: &Path,
        source: Arc<dyn Source>,
        threads: u32,
        watch: Arc<Watch>,
        anchor: Anchor,
    ) -> Result<Self, MountError> {
        let log = state.join("journal.log");
        let journal = Arc::new(Journal::open(&log).map_err(io("open", &log))?);
        let provider = Arc::new(Provider::new(source, journal));
        let instance = sys::Instance::start(root, provider.clone(), threads)?;
        Ok(Self {
            root: root.to_owned(),
            state: state.to_owned(),
            provider,
            instance: Some(instance),
            watch,
            anchor: std::sync::Mutex::new(anchor),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn state(&self) -> &Path {
        &self.state
    }
    pub fn provider(&self) -> &Arc<Provider> {
        &self.provider
    }
    /// The running instance, for placeholder updates.
    pub fn instance(&self) -> &sys::Instance {
        self.instance.as_ref().expect("a mounted projection runs")
    }

    /// The mount's change tracker.
    pub fn watch(&self) -> &Arc<Watch> {
        &self.watch
    }

    /// Records that everything before the kept position in the change
    /// journal is committed: a later provider starts reading there, with
    /// the files that were open for writing then still suspect.
    pub fn settle(&self, kept: &Kept) -> std::io::Result<()> {
        let mut anchor = self
            .anchor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(position) = kept.position {
            anchor.position = position;
        }
        anchor.open.clone_from(&kept.open);
        anchor.write(&self.state)
    }

    /// The paths and listings to compare for `pending`. When the journal
    /// could not answer, that is everything on disk: every directory's
    /// names, every placeholder whose facts differ from the base's, and
    /// every full file `known` does not vouch for. `known` is not asked
    /// when the journal its answers rest on is not the one read now.
    ///
    /// A placeholder's bytes are the base's for the path it was made at. A
    /// placeholder renamed over another name still has the size and time of
    /// the file it was made for, which is how it is told here; two files of
    /// one size and one time cannot be told apart this way.
    pub fn dirty(&self, pending: &Pending, known: Known<'_>) -> Result<Dirty, StatusError> {
        let mut dirty = self.watch.examine(&self.root, pending)?;
        if !pending.everything {
            return Ok(dirty);
        }
        let mut directories = vec![String::new()];
        {
            let _local = self.provider.local_only();
            let mut local = Local::new(&self.root);
            let mut walk = vec![String::new()];
            while let Some(directory) = walk.pop() {
                let base = match self.provider.tree.resolve(&directory)? {
                    Some(found) if found.is_directory() => {
                        Some(self.provider.tree.directory(found.path())?)
                    }
                    _ => None,
                };
                for item in local.list(&directory)?.unwrap_or_default() {
                    let path = join(&directory, &item.name);
                    if item.link() {
                        dirty.paths.insert(path);
                    } else if item.directory {
                        walk.push(path.clone());
                        directories.push(path);
                    } else if local
                        .state(&path)?
                        .is_none_or(|bits| bits & sys::state::FULL != 0)
                    {
                        if pending.unvouched || !known(&path, &item) {
                            dirty.sure.insert(path);
                        }
                    } else {
                        match base.as_ref().and_then(|base| base.find(&item.name)) {
                            Some(entry)
                                if (entry.size, entry.written) == (item.size, item.written) =>
                            {
                                if (entry.attributes ^ item.attributes) & attribute::READ_ONLY != 0
                                {
                                    dirty.paths.insert(path);
                                }
                            }
                            // Made for another path, or for a base that is
                            // not this one.
                            _ => {
                                dirty.sure.insert(path);
                            }
                        }
                    }
                }
            }
        }
        for directory in directories {
            let native = native(&self.root, &directory);
            if let Ok(volume) = Volume::open(&native) {
                self.watch.know(volume.directory()?.index);
            }
            if let Some(names) = watch::names(&native)? {
                dirty.listings.insert(directory, names);
            }
        }
        Ok(dirty)
    }

    /// What this mount holds on the local disk now.
    pub fn usage(&self) -> Result<status::Usage, StatusError> {
        let _local = self.provider.local_only();
        status::usage(&mut Local::new(&self.root))
    }

    /// What changed against the source, and the links found on the way.
    /// `known` vouches for full files the caller knows to be the base's.
    pub fn report(&self, known: Known<'_>) -> Result<Report, StatusError> {
        let recorded = self.provider.journal.recorded();
        let (pending, _) = self.watch.pending();
        let dirty = self.dirty(&pending, known)?;
        let _local = self.provider.local_only();
        status::changes(&self.provider, Local::new(&self.root), &recorded, &dirty)
    }

    /// What changed against the source.
    pub fn status(&self) -> Result<Vec<Change>, StatusError> {
        Ok(self.report(&|_, _| false)?.changes)
    }

    /// Stops serving. The root, its local items and the state directory
    /// stay, for [`Self::resume`]; nothing is committed or removed.
    pub fn unmount(mut self) {
        self.watch.stop();
        self.instance = None;
    }

    /// Stops serving and removes the root and the state directory: every
    /// uncommitted change is gone.
    pub fn discard(mut self) -> Result<(), MountError> {
        // Nothing is removed from a directory the state does not belong to.
        let anchor = self
            .anchor
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        Self::belongs(&self.root, &anchor)?;
        self.watch.stop();
        // Once no provider runs, some items cannot be deleted (a placeholder
        // that was never hydrated, an empty hydrated file: error 369). So
        // the files on disk are removed first, while this provider still
        // serves and shows its own process only the disk. The provider's
        // own delete leaves no tombstone and raises no notification, one
        // operation per file. What it does not take (tombstones, which are
        // invisible until the stop, and the directories that hold them) is
        // removed after the stop, when it can be.
        {
            let _local = self.provider.local_only();
            let mut local = Local::new(&self.root);
            let flags = sys::ALLOW_DIRTY_METADATA
                | sys::ALLOW_DIRTY_DATA
                | sys::ALLOW_TOMBSTONE
                | sys::ALLOW_READ_ONLY;
            let mut pending = vec![String::new()];
            while let Some(directory) = pending.pop() {
                let items = local
                    .list(&directory)
                    .map_err(io("list", &self.root))?
                    .unwrap_or_default();
                for item in items {
                    let path = crate::provider::tree::join(&directory, &item.name);
                    if item.directory {
                        pending.push(path);
                        continue;
                    }
                    let name = path.replace('/', "\\");
                    if !matches!(self.instance().delete_item(&name, flags), Ok(true)) {
                        // Not the provider's to delete: an ordinary delete.
                        // A failure is left for the pass below to report.
                        let _ =
                            std::fs::remove_file(crate::track::local::native(&self.root, &path));
                    }
                }
            }
        }
        self.instance = None;
        remove_tree(&self.root).map_err(io("remove", &self.root))?;
        remove_tree(&self.state).map_err(io("remove", &self.state))
    }
}
