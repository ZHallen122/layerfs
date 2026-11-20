//! Commit: capture the changes the volume's change journal marks into the
//! bound Workspace, publish through the engine's existing Commit, then move
//! the projection's base to the root the engine installed.
//!
//! Publication, merge with a moved Branch head and Conflict detection are
//! the engine's (`BoundWorkspace::commit_captured`); nothing here decides
//! them. When the engine installs a root that is not the one this Workspace
//! captured (a merge, clean or conflicted), the directory is brought to it
//! (`follow.rs`). A conflicted merge publishes nothing: the engine installs
//! the merged files with marker blocks in the Workspace view, and they
//! reach the local directory the same way, for the user to resolve, as a
//! FUSE mount would show them.
use super::capture::{capture, CaptureError, Captured};
use super::follow::{follow, retry, Rebased};
use super::ledger::{Ledger, Stored};
use super::mount::{Workspace, WorkspaceError};
use crate::provider::tree::Tree;
use crate::source::snapshot::SnapshotSource;
use crate::sys;
use crate::track::journal::Recorded;
use crate::track::local::{native, LocalItem};
use crate::track::watch::Pending;
use layerfs_content::ObjectId;
use layerfs_history::CommitOutcome;
use layerfs_store_service::history::lookup::hex;
use layerfs_store_service::store::CommitOptions;
use std::sync::Arc;

/// What a Commit published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// A new Commit on the Branch.
    Committed { commit: String, root: String },
    /// Nothing differed from the Branch's head.
    UpToDate { head: Option<String>, root: String },
}

/// A successful Commit.
#[derive(Clone, Debug)]
pub struct Committed {
    pub outcome: Outcome,
    /// True when the Branch head had moved and the engine merged it in.
    pub merged: bool,
    pub captured: Captured,
    pub rebased: Rebased,
    /// What failed after the Commit was published. The Commit stands; the
    /// local directory or its records may be behind, and each line says
    /// what to do.
    pub warnings: Vec<String>,
}

/// Why a Commit published nothing.
#[derive(Debug)]
pub enum CommitRefusal {
    /// The local changes could not be captured.
    Capture(CaptureError),
    /// The Branch head moved and changed paths this Workspace changed too.
    /// The conflicted files now hold marker blocks in the directory.
    Conflict {
        head: Option<String>,
        conflicts: Vec<String>,
        written: Vec<String>,
        /// What the directory did to follow the merge.
        rebased: Box<Rebased>,
    },
    /// An earlier Commit or Conflict installed a new base and the directory
    /// could not follow it. Unmount and resume: the next provider finishes
    /// the step.
    Behind(String),
    /// The engine refused; the Workspace is unchanged and may commit again.
    Failed(String),
    /// The Branch moved while no provider served this Workspace. The engine
    /// can bind a Workspace only at a Branch's present head, so the merge
    /// base is gone and a Commit could silently undo the Branch's changes.
    Diverged { recorded: String, head: String },
    /// The outcome is not known; the engine keeps the Workspace's custody
    /// and nothing is retried.
    Unknown(String),
}

impl std::fmt::Display for CommitRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capture(error) => write!(f, "{error}"),
            Self::Conflict { conflicts, .. } => write!(
                f,
                "Conflict: the head moved and {} path(s) conflict; nothing was committed",
                conflicts.len()
            ),
            Self::Failed(text) => write!(f, "Failed: {text}"),
            Self::Behind(text) => write!(f, "Failed: {text}"),
            Self::Diverged { recorded, head } => write!(
                f,
                "Diverged: the Branch moved from {recorded} to {head} while this Workspace \
                 was not served; it cannot be merged after a restart, and nothing was committed"
            ),
            Self::Unknown(text) => write!(f, "Unknown: {text}"),
        }
    }
}
impl std::error::Error for CommitRefusal {}

/// What a caller is told about the items the directory could not bring to
/// a new base.
fn told(rebased: &Rebased) -> Vec<String> {
    let kept = rebased.kept.iter().map(|path| {
        format!(
            "{path} changed after the Commit read it, and the merge changed it too: the \
             directory keeps your file, and the next Commit stores it over the merged one"
        )
    });
    let refused = rebased.refused.iter().map(|(path, why)| {
        format!(
            "{path} could not be brought to the new base ({why}): it shows the old one. \
             Status lists it as behind, and every status and Commit tries again"
        )
    });
    kept.chain(refused).collect()
}

/// What a Commit says of a path that is still behind when it begins.
fn still_behind(path: &str) -> String {
    format!(
        "{path} still shows an older version than the Branch holds: an earlier merge could \
         not rewrite it, and neither could this Commit. It is tried again at every status \
         and Commit; do not edit it before"
    )
}

impl Workspace {
    pub(super) fn source_at(&self, root: ObjectId) -> Result<Arc<SnapshotSource>, WorkspaceError> {
        SnapshotSource::open(self.engine.store(), self.stack.scope, root)
            .map(Arc::new)
            .map_err(|e| WorkspaceError::Failed(format!("base tree: {e}")))
    }

    /// Serves the root the engine has now installed. `replaced` is set when
    /// that root is not the one this Workspace captured: the tree the
    /// directory shows, and the tree installed in its place. The directory
    /// is then brought from the one to the other.
    ///
    /// The state directory is told first, before anything on disk changes:
    /// which base is served from now on, and which step is unfinished. A
    /// provider that stops in between leaves a directory the next one can
    /// finish, not one whose base nobody knows.
    fn advance(
        &self,
        replaced: Option<(ObjectId, ObjectId)>,
        stamps: &mut Ledger,
    ) -> Result<Rebased, WorkspaceError> {
        let snapshot = self
            .bound
            .snapshot()
            .map_err(|e| WorkspaceError::Failed(format!("binding: {e}")))?;
        let source = self.source_at(snapshot.effective_root)?;
        self.write_record(replaced)?;
        // The new base is served from here: a file first opened from now on
        // is projected from it.
        self.projection.provider().tree.replace(source);
        let Some((shown, installed)) = replaced else {
            return Ok(Rebased::default());
        };
        let rebased = self.follow(shown, installed, stamps)?;
        self.write_record(None)?;
        Ok(rebased)
    }

    /// Brings the directory from the tree `shown` to the tree `installed`.
    pub(super) fn follow(
        &self,
        shown: ObjectId,
        installed: ObjectId,
        stamps: &mut Ledger,
    ) -> Result<Rebased, WorkspaceError> {
        let shown = Tree::new(self.source_at(shown)?);
        let target = Tree::new(self.source_at(installed)?);
        let behind = self
            .unfollowed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        follow(self, &shown, &target, stamps, &behind)
    }

    /// Tries again every path an earlier merge could not bring over, and
    /// returns the paths that are still behind. A path leaves the list when
    /// the item on disk holds the bytes of the tree it has to reach.
    pub(super) fn catch_up(&self, stamps: &mut Ledger) -> Vec<String> {
        let mut list = self
            .unfollowed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if list.is_empty() {
            return Vec::new();
        }
        for ((shown, target), paths) in list.groups() {
            let tried = (|| {
                let shown =
                    Tree::new(self.source_at(super::mount::root_of(&shown, "shown root")?)?);
                let target =
                    Tree::new(self.source_at(super::mount::root_of(&target, "target root")?)?);
                retry(self, &shown, &target, &paths, stamps)
            })();
            // A tree that cannot be read now is tried again next time.
            let Ok(tried) = tried else { continue };
            for path in &paths {
                let stays = tried
                    .kept
                    .iter()
                    .any(|kept| kept.eq_ignore_ascii_case(path))
                    || tried
                        .refused
                        .iter()
                        .any(|(refused, _)| refused.eq_ignore_ascii_case(path));
                if !stays {
                    list.remove(path);
                }
            }
        }
        // Best effort: a list that is not written is tried from the older
        // one, which finds these paths brought over.
        let _ = list.save(self.projection.state());
        list.paths()
    }

    /// Keeps the list of paths that are behind true to a Commit: what the
    /// capture compared equals the view and is no longer behind; after a
    /// merge every path still behind has to reach the installed tree, and
    /// what the merge step could not change joins the list.
    fn unfollow(
        &self,
        replaced: Option<(ObjectId, ObjectId)>,
        captured: &Captured,
        rebased: &Rebased,
    ) -> std::io::Result<()> {
        let mut list = self
            .unfollowed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if list.is_empty() && rebased.refused.is_empty() {
            return Ok(());
        }
        for path in captured.removed.iter().chain(&captured.compared) {
            list.remove(path);
        }
        if let Some((shown, installed)) = replaced {
            let (shown, installed) = (hex(shown.as_bytes()), hex(installed.as_bytes()));
            list.retarget(&installed);
            for (path, _) in &rebased.refused {
                list.add(path, &shown, &installed);
            }
        }
        list.save(self.projection.state())
    }

    /// Enters what the capture read into `stamps`: each file as it was when
    /// its bytes reached the view, which is what this Workspace holds at
    /// that path whatever a merge then makes of it.
    fn note(stamps: &mut Ledger, captured: &Captured) {
        for path in &captured.removed {
            stamps.forget(path);
        }
        // No tree is read for this: the size, time and journal number a
        // file had when the capture read it are the whole record.
        for seen in &captured.files {
            stamps.record(
                &seen.path,
                Stored {
                    size: seen.size,
                    written: seen.written,
                    change: seen.change,
                },
            );
        }
    }

    /// Clears the tombstones of the names a published Commit removed.
    fn clear(&self, captured: &Captured) {
        let root = self.projection.root();
        for path in &captured.removed {
            // The deletion is committed: the base no longer holds the name,
            // so the marker that hid it has nothing left to hide. Left on
            // disk it would hide the name if another Workspace brings it
            // back; a later merge clears it then, when this fails now.
            // Something else may be at the path by now; that stays.
            if std::fs::symlink_metadata(native(root, path)).is_ok() {
                continue;
            }
            let _ = self
                .projection
                .instance()
                .delete_item(&path.replace('/', "\\"), sys::ALLOW_TOMBSTONE);
        }
    }

    /// Commits the Workspace's changes to its Branch: one capture, one
    /// engine Commit, no retry. What the change journal records after the
    /// capture began is left for the next Commit.
    pub fn commit(&self, message: Option<&str>) -> Result<Committed, CommitRefusal> {
        let _one = self
            .committing
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some((recorded, head)) = &self.diverged {
            return Err(CommitRefusal::Diverged {
                recorded: recorded.clone(),
                head: head.clone(),
            });
        }
        if let Some(why) = self.behind() {
            return Err(CommitRefusal::Behind(why));
        }
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // First what an earlier merge left behind: a file brought over now
        // is not an old file the capture could take for a change.
        let behind = self.catch_up(&mut ledger);
        let recorded = self.projection.provider().journal.recorded();
        // Everything before the taken position in the change journal is in
        // `taken`; what comes after stays with the tracker.
        let taken = self.projection.watch().take();
        let mut published = self.publish(message, &recorded, &taken.pending, &mut ledger);
        match &mut published {
            // Published: a failure from here on does not undo it, and is
            // told beside it instead of in its place.
            Ok(committed) => {
                committed
                    .warnings
                    .extend(behind.iter().map(|path| still_behind(path)));
                // A file the capture found open for writing stays suspect:
                // its handle may write again without a journal record.
                let kept = self
                    .projection
                    .watch()
                    .settle(&taken, &committed.captured.held);
                if let Err(error) = self.projection.settle(&kept) {
                    committed.warnings.push(format!(
                        "the change journal position was not recorded ({error}): \
                         after a restart everything on disk is compared once"
                    ));
                }
                if let Err(error) = ledger.save(self.projection.state()) {
                    committed.warnings.push(format!(
                        "the record of committed files was not written ({error}): \
                         after a restart committed files are compared by their bytes"
                    ));
                }
            }
            // Nothing is known to be published: every item stays suspect.
            Err(_) => self.projection.watch().restore(taken),
        }
        published
    }

    fn publish(
        &self,
        message: Option<&str>,
        recorded: &Recorded,
        pending: &Pending,
        ledger: &mut Ledger,
    ) -> Result<Committed, CommitRefusal> {
        let provider = self.projection.provider();
        let captured = {
            let root = self.projection.root();
            let known =
                |path: &str, item: &LocalItem| ledger.vouches(&native(root, path), path, item);
            let dirty = self
                .projection
                .dirty(pending, &known)
                .map_err(|e| CommitRefusal::Failed(format!("changed items: {e}")))?;
            capture(
                &self.bound,
                provider,
                self.projection.root(),
                recorded,
                &dirty,
            )
            .map_err(CommitRefusal::Capture)?
        };
        let success = self
            .bound
            .commit_captured(&CommitOptions {
                force: false,
                message: Some(message.unwrap_or("Workspace Commit").to_owned()),
            })
            .map_err(|failure| {
                let settled = failure.locally_settled
                    && failure.local_error.is_none()
                    && failure
                        .namespace
                        .as_ref()
                        .is_none_or(|held| !held.retained());
                if settled && !failure.error.uncertain() {
                    CommitRefusal::Failed(failure.to_string())
                } else {
                    CommitRefusal::Unknown(failure.to_string())
                }
            })?;
        let merged = success.merge.is_some();
        let mut warnings = Vec::new();
        // The engine installed a root: the Commit's, or a merge's. What
        // the capture read goes into the record of committed files first,
        // so that the directory can tell a file it captured from one that
        // changed since. Without a publication the files are not
        // committed: the record is tried on a copy and keeps only what it
        // agrees with.
        let followed = if success.history.is_some() {
            Self::note(ledger, &captured);
            self.advance(success.replaced, ledger)
        } else {
            let mut tried = ledger.clone();
            Self::note(&mut tried, &captured);
            let followed = self.advance(success.replaced, &mut tried);
            ledger.keep_same(&tried);
            followed
        };
        let rebased = match followed {
            Ok(rebased) => rebased,
            Err(error) => {
                let why = format!(
                    "the directory did not follow the new base ({error}): unmount and \
                     resume, and the next provider finishes the step"
                );
                self.fall_behind(&why);
                if success.history.is_none() {
                    return Err(CommitRefusal::Behind(why));
                }
                warnings.push(why);
                Rebased::default()
            }
        };
        warnings.extend(told(&rebased));
        if let Err(error) = self.unfollow(success.replaced, &captured, &rebased) {
            warnings.push(format!(
                "the list of paths that are behind was not written ({error}): after a \
                 restart they are not tried again"
            ));
        }
        let Some(history) = success.history else {
            let merge = success.merge.unwrap_or_else(unreachable_merge);
            return Err(CommitRefusal::Conflict {
                head: merge.head.map(|id| hex(id.as_slice())),
                conflicts: merge.conflicts.iter().map(|c| c.path.clone()).collect(),
                written: merge.written,
                rebased: Box::new(rebased),
            });
        };
        // Published. A file still open for writing is compared again at
        // the next Commit: the tracker keeps it until the journal shows
        // its handle closed.
        self.clear(&captured);
        if let Err(error) = provider.journal.forget_through(recorded.sequence) {
            warnings.push(format!(
                "the reported renames were not cleared ({error}): they are replayed, \
                 and found already done, at the next Commit"
            ));
        }
        let outcome = match history {
            CommitOutcome::Committed { record, .. } => Outcome::Committed {
                commit: hex(record.id.as_slice()),
                root: hex(record.root.as_bytes()),
            },
            CommitOutcome::UpToDate { head, root } => Outcome::UpToDate {
                head: head.map(|id| hex(id.as_slice())),
                root: hex(root.as_bytes()),
            },
        };
        Ok(Committed {
            outcome,
            merged,
            captured,
            rebased,
            warnings,
        })
    }
}

/// The engine answers a Commit without a publication only for a
/// conflicted merge, and reports that merge.
fn unreachable_merge() -> layerfs_store_service::store::MergeReport {
    layerfs_store_service::store::MergeReport {
        from: None,
        head: None,
        conflicts: Vec::new(),
        lww: Vec::new(),
        written: Vec::new(),
        written_truncated: false,
        lane_paths: None,
    }
}
