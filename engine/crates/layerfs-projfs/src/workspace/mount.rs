//! The entry point: a LayerFS Workspace mounted in a local Windows
//! directory. Mount, status, Commit, unmount and discard mirror the SDK's
//! Workspace operations; the mount is a ProjFS projection instead of a FUSE
//! mount inside a Sandbox, and needs no container.
use super::ledger::Ledger;
use super::record::Record;
use super::unfollowed::Unfollowed;
use crate::projection::{MountError, Projection};
use crate::source::snapshot::SnapshotSource;
use crate::store::{Engine, EngineError};
use crate::track::local::LocalItem;
use crate::track::status::{Change, StatusError};
use layerfs_content::ObjectId;
use layerfs_history::{BranchRecord, LayerStackRecord};
use layerfs_store_service::history::lookup::{hex, unhex};
use layerfs_store_service::store::BoundWorkspace;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What a Workspace is mounted from.
#[derive(Clone, Copy, Debug)]
pub enum From<'a> {
    /// The Branch's present head.
    Branch(&'a str),
    /// A Commit (or Layer) identity. A Workspace always commits to a
    /// Branch, so a new Branch `branch` is forked at it first, with the
    /// existing fork, and mounted.
    Commit { commit: &'a str, branch: &'a str },
}

/// One mount request.
#[derive(Clone, Copy, Debug)]
pub struct MountRequest<'a> {
    pub project: &'a str,
    pub from: From<'a>,
    /// An empty or absent local directory on an NTFS volume.
    pub root: &'a Path,
    /// Where the mount keeps its local records; outside `root`. `None`
    /// selects the sibling directory `<root>.layerfs`.
    pub state: Option<&'a Path>,
    /// Provider worker threads; 0 lets ProjFS choose.
    pub threads: u32,
}

/// Why a Workspace call was refused.
#[derive(Debug)]
pub enum WorkspaceError {
    Engine(EngineError),
    Mount(MountError),
    Status(StatusError),
    /// The Workspace or its records failed; the text is the cause.
    Failed(String),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(error) => write!(f, "{error}"),
            Self::Mount(error) => write!(f, "{error}"),
            Self::Status(error) => write!(f, "Failed: status: {error}"),
            Self::Failed(text) => write!(f, "Failed: {text}"),
        }
    }
}
impl std::error::Error for WorkspaceError {}
impl std::convert::From<EngineError> for WorkspaceError {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}
impl std::convert::From<MountError> for WorkspaceError {
    fn from(error: MountError) -> Self {
        Self::Mount(error)
    }
}
impl std::convert::From<StatusError> for WorkspaceError {
    fn from(error: StatusError) -> Self {
        Self::Status(error)
    }
}

/// What a mounted Workspace reports.
#[derive(Clone, Debug)]
pub struct Status {
    pub branch: String,
    /// The Commit the Workspace is based on; `None` on a Branch without one.
    pub head: Option<String>,
    /// The base tree's root identity.
    pub root: String,
    /// Uncommitted changes, sorted by path.
    pub changes: Vec<Change>,
    /// Paths the mount cannot show or cannot commit, each with the
    /// reason: base names Windows cannot project, and junctions or symbolic
    /// links on disk, which are never followed.
    pub hidden: Vec<(String, &'static str)>,
    /// Set when the Branch moved while no provider served this Workspace:
    /// the recorded base and the Branch's present head. The directory still
    /// shows the recorded base, and Commit is refused.
    pub diverged: Option<(String, String)>,
    /// Paths that still show an older version than the Branch holds: a
    /// merge could not rewrite them (another process held the file). They
    /// are no uncommitted change, and must not be edited as they are. Each
    /// status and Commit tries to bring them over; they are also in
    /// `hidden`, with the reason.
    pub behind: Vec<String>,
}

/// The default state directory of a mount at `root`.
pub fn default_state(root: &Path) -> PathBuf {
    let mut name = root.file_name().unwrap_or_default().to_os_string();
    name.push(".layerfs");
    root.with_file_name(name)
}

/// A mounted Workspace.
pub struct Workspace {
    pub(super) engine: Arc<Engine>,
    pub(super) stack: LayerStackRecord,
    pub(super) branch: String,
    pub(super) bound: Arc<BoundWorkspace>,
    pub(super) projection: Projection,
    /// One Commit at a time, as the engine requires of a Workspace.
    pub(super) committing: Mutex<()>,
    /// What each full file on disk was last committed as.
    pub(super) ledger: Mutex<Ledger>,
    /// The recorded base Commit and the Branch's present head, when a
    /// restart found them different.
    pub(super) diverged: Option<(String, String)>,
    /// Set when the directory could not follow a base the engine
    /// installed: Commit is refused until a resume finished the step.
    pub(super) behind: Mutex<Option<String>>,
    /// The paths a merge could not bring over.
    pub(super) unfollowed: Mutex<Unfollowed>,
}

pub(super) fn root_of(text: &str, what: &str) -> Result<ObjectId, WorkspaceError> {
    unhex(text)
        .and_then(|bytes| ObjectId::from_bytes(&bytes).ok())
        .ok_or_else(|| WorkspaceError::Failed(format!("recorded {what}")))
}

impl Workspace {
    /// Mounts a Workspace of `request.project` in `request.root`. Binding
    /// reads the Branch's root only: no tree is scanned, copied or
    /// materialized, and the directory is populated on demand.
    pub fn mount(engine: &Arc<Engine>, request: MountRequest<'_>) -> Result<Self, WorkspaceError> {
        let stack = engine.project(request.project)?;
        let name = match request.from {
            From::Branch(name) => name,
            From::Commit { commit, branch } => {
                layerfs_store_service::history::branch::fork(
                    engine.store(),
                    &stack,
                    branch,
                    Some(commit),
                )
                .map_err(EngineError::from)?;
                branch
            }
        };
        let branch = engine.branch(&stack, name)?;
        let state = request
            .state
            .map_or_else(|| default_state(request.root), Path::to_owned);
        Self::start(engine, stack, &branch, None, |source| {
            Projection::mount(request.root, &state, source, request.threads)
        })
    }

    /// Serves again a Workspace directory whose provider stopped (by
    /// unmount, or by dying): the local files stay as they were. Returns
    /// the Workspace and how many uncommitted changes it has.
    ///
    /// The engine binds a Workspace only at its Branch's present head. When
    /// the head is still the recorded one, nothing was lost. When the
    /// Branch moved meanwhile, the merge base is gone: the directory keeps
    /// showing the recorded base, status is told against it, and Commit is
    /// refused, because it could not tell this Workspace's changes from
    /// the Branch's and would undo them.
    pub fn resume(
        engine: &Arc<Engine>,
        root: &Path,
        state: Option<&Path>,
        threads: u32,
    ) -> Result<(Self, usize), WorkspaceError> {
        let state = state.map_or_else(|| default_state(root), Path::to_owned);
        let record = Record::read(&state).map_err(WorkspaceError::Failed)?;
        let stack = engine.project(&record.project)?;
        let branch = engine.branch(&stack, &record.branch)?;
        let workspace = Self::start(engine, stack, &branch, Some(&record), |source| {
            Ok(Projection::resume(root, &state, source, threads)?.0)
        })?;
        match workspace.finish(&record).and_then(|()| workspace.status()) {
            Ok(status) => Ok((workspace, status.changes.len())),
            // Not served half-followed: the directory and the state stay
            // as they are for another attempt.
            Err(error) => Err(match workspace.unmount() {
                Ok(()) => error,
                Err(release) => WorkspaceError::Failed(format!("{error}; then {release}")),
            }),
        }
    }

    /// Finishes a step an earlier provider recorded and did not complete:
    /// bringing the directory from the tree it showed to the tree a merge
    /// installed.
    fn finish(&self, record: &Record) -> Result<(), WorkspaceError> {
        if record.shown.is_empty() {
            return Ok(());
        }
        let (shown, target) = (
            root_of(&record.shown, "shown root")?,
            root_of(&record.target, "target root")?,
        );
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let rebased = self.follow(shown, target, &mut ledger)?;
        if !rebased.refused.is_empty() {
            let mut list = self
                .unfollowed
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            for (path, _) in &rebased.refused {
                list.add(path, &record.shown, &record.target);
            }
            list.save(self.projection.state()).map_err(|error| {
                WorkspaceError::Failed(format!("list of paths behind: {error}"))
            })?;
        }
        // Kept as recorded but for the step: after a restart that found the
        // Branch moved, the record is not this binding's to rewrite.
        Record {
            shown: String::new(),
            target: String::new(),
            ..record.clone()
        }
        .write(self.projection.state())
        .map_err(WorkspaceError::Failed)?;
        // Best effort: an entry that is missing is a comparison later.
        let _ = ledger.save(self.projection.state());
        Ok(())
    }

    fn start(
        engine: &Arc<Engine>,
        stack: LayerStackRecord,
        branch: &BranchRecord,
        resumed: Option<&Record>,
        project: impl FnOnce(Arc<SnapshotSource>) -> Result<Projection, MountError>,
    ) -> Result<Self, WorkspaceError> {
        let bound = engine.bind(branch)?;
        let mut diverged = None;
        let mounted = (|| {
            let snapshot = bound
                .snapshot()
                .map_err(|e| WorkspaceError::Failed(format!("binding: {e}")))?;
            let head = snapshot
                .branch
                .head_commit
                .map(|id| hex(id.as_slice()))
                .unwrap_or_default();
            let mut root = snapshot.effective_root;
            if let Some(record) = resumed.filter(|record| record.head != head) {
                // The directory was projected from the recorded base and
                // keeps showing it.
                root = root_of(&record.root, "base root")?;
                diverged = Some((record.head.clone(), head));
            }
            let source = SnapshotSource::open(engine.store(), snapshot.scope, root)
                .map_err(|e| WorkspaceError::Failed(format!("base tree: {e}")))?;
            Ok(project(Arc::new(source))?)
        })();
        let projection = match mounted {
            Ok(projection) => projection,
            Err(error) => {
                // The binding had no effect yet; it is closed with its cause kept.
                return Err(match engine.release(&bound) {
                    Ok(()) => error,
                    Err(release) => WorkspaceError::Failed(format!("{error}; then {release}")),
                });
            }
        };
        let (ledger, unfollowed) = match resumed {
            Some(_) => (
                Ledger::load(projection.state()),
                Unfollowed::load(projection.state()),
            ),
            None => (Ledger::default(), Unfollowed::default()),
        };
        let workspace = Self {
            engine: engine.clone(),
            stack,
            branch: branch.name.as_str().to_owned(),
            bound,
            projection,
            committing: Mutex::new(()),
            ledger: Mutex::new(ledger),
            diverged,
            behind: Mutex::new(None),
            unfollowed: Mutex::new(unfollowed),
        };
        // A resumed Workspace's record is rewritten once its unfinished
        // step, if it has one, is done.
        if resumed.is_none() {
            workspace.write_record(None)?;
        }
        Ok(workspace)
    }

    pub(super) fn behind(&self) -> Option<String> {
        self.behind
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub(super) fn fall_behind(&self, why: &str) {
        *self
            .behind
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(why.to_owned());
    }

    /// The mounted directory.
    pub fn root(&self) -> &Path {
        self.projection.root()
    }
    /// The mount's state directory.
    pub fn state(&self) -> &Path {
        self.projection.state()
    }
    /// The projection, for its served-request counters.
    pub fn projection(&self) -> &Projection {
        &self.projection
    }

    /// Records the base the binding has now. `following` is the step in
    /// progress, when there is one: the tree the directory shows, and the
    /// tree it is being brought to.
    pub(super) fn write_record(
        &self,
        following: Option<(ObjectId, ObjectId)>,
    ) -> Result<(), WorkspaceError> {
        let snapshot = self
            .bound
            .snapshot()
            .map_err(|e| WorkspaceError::Failed(format!("binding: {e}")))?;
        let (shown, target) = following
            .map(|(shown, target)| (hex(shown.as_bytes()), hex(target.as_bytes())))
            .unwrap_or_default();
        Record {
            project: self.stack.name.as_str().to_owned(),
            branch: self.branch.clone(),
            root: hex(snapshot.effective_root.as_bytes()),
            head: snapshot
                .branch
                .head_commit
                .map(|id| hex(id.as_slice()))
                .unwrap_or_default(),
            shown,
            target,
        }
        .write(self.projection.state())
        .map_err(WorkspaceError::Failed)
    }

    /// The Workspace's uncommitted changes against its base.
    pub fn status(&self) -> Result<Status, WorkspaceError> {
        let snapshot = self
            .bound
            .snapshot()
            .map_err(|e| WorkspaceError::Failed(format!("binding: {e}")))?;
        let (report, behind) = {
            let mut ledger = self
                .ledger
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            // What an earlier merge could not bring over is tried first, so
            // that what follows describes the directory as it then is.
            let behind = self.catch_up(&mut ledger);
            let root = self.projection.root();
            let report = self.projection.report(&|path: &str, item: &LocalItem| {
                ledger.vouches(&crate::track::local::native(root, path), path, item)
            })?;
            (report, behind)
        };
        let mut hidden = self.projection.provider().tree.hidden();
        hidden.extend(report.links.into_iter().map(|path| {
            (
                path,
                "a junction or symbolic link: never followed, not committed",
            )
        }));
        hidden.extend(behind.iter().map(|path| {
            (
                path.clone(),
                "behind the Branch: a merge could not rewrite it; tried again at every status \
                 and Commit",
            )
        }));
        Ok(Status {
            branch: self.branch.clone(),
            head: snapshot.branch.head_commit.map(|id| hex(id.as_slice())),
            root: hex(snapshot.effective_root.as_bytes()),
            changes: report.changes,
            hidden,
            diverged: self.diverged.clone(),
            behind,
        })
    }

    /// Stops serving and closes the binding. The directory, its local
    /// files and the state stay for [`Self::resume`]; nothing is committed
    /// and no history is removed.
    pub fn unmount(self) -> Result<(), WorkspaceError> {
        let Self {
            engine,
            bound,
            projection,
            ..
        } = self;
        projection.unmount();
        Ok(engine.release(&bound)?)
    }

    /// Stops serving, closes the binding and removes the directory and the
    /// state: every uncommitted change is gone. History is untouched.
    pub fn discard(self) -> Result<(), WorkspaceError> {
        let Self {
            engine,
            bound,
            projection,
            ..
        } = self;
        let removed = projection.discard();
        engine.release(&bound)?;
        Ok(removed?)
    }
}
