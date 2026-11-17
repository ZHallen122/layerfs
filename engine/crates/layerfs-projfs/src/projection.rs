//! One mounted projection of a source into a local directory: start, status,
//! unmount, discard. The Workspace entry point composes this with the Store.
use crate::provider::Provider;
use crate::source::Source;
use crate::sys;
use crate::track::local::{verbatim, Local};
use crate::track::status::{self, Change, StatusError};
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
    /// The scan of the local disk before a resume failed.
    Scan(String),
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
            Self::Scan(text) => write!(f, "Failed: scan of the local disk: {text}"),
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

/// A source projected into a local directory by a running provider.
pub struct Projection {
    root: PathBuf,
    state: PathBuf,
    provider: Arc<Provider>,
    instance: Option<sys::Instance>,
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
        Self::start(root, state, source, threads)
    }

    /// Serves again a root a provider stopped serving, by unmount or by
    /// dying. The items on disk stay as they are and the journal file is
    /// replayed. Changes made while no provider ran were never reported, so
    /// the local disk is scanned first, before anything is served; the
    /// paths it finds join the journal. Returns how many it found.
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
        let found = status::scan(root, &crate::provider::Tree::new(source.clone()))
            .map_err(|error| MountError::Scan(error.to_string()))?;
        let resumed = Self::start(root, state, source, threads)?;
        found
            .iter()
            .for_each(|path| resumed.provider.journal.touch(path));
        Ok((resumed, found.len()))
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

    /// Journals the files whose read-only attribute was changed: the one
    /// change ProjFS never reports. Returns how many it found.
    pub fn sweep(&self) -> Result<usize, StatusError> {
        let found = {
            let _local = self.provider.local_only();
            status::attribute_changes(&self.provider, &mut Local::new(&self.root))?
        };
        found
            .iter()
            .for_each(|path| self.provider.journal.touch(path));
        Ok(found.len())
    }

    /// What this mount holds on the local disk now.
    pub fn usage(&self) -> Result<status::Usage, StatusError> {
        let _local = self.provider.local_only();
        status::usage(&mut Local::new(&self.root))
    }

    /// What changed against the source, from the journal's paths.
    pub fn status(&self) -> Result<Vec<Change>, StatusError> {
        self.sweep()?;
        let recorded = self.provider.journal.recorded();
        let _local = self.provider.local_only();
        status::changes(&self.provider, Local::new(&self.root), &recorded)
    }

    /// Stops serving. The root, its local items and the state directory
    /// stay, for [`Self::resume`]; nothing is committed or removed.
    pub fn unmount(mut self) {
        self.instance = None;
    }

    /// Stops serving and removes the root and the state directory: every
    /// uncommitted change is gone.
    pub fn discard(mut self) -> Result<(), MountError> {
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
