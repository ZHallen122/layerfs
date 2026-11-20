//! Commit: capture the changes the volume's change journal marks into the
//! bound Workspace, publish through the engine's existing Commit, then move
//! the projection's base to the root the engine installed.
//!
//! Publication, merge with a moved Branch head and Conflict detection are
//! the engine's (`BoundWorkspace::commit_captured`); nothing here decides
//! them. A conflicted merge publishes nothing: the engine installs the
//! merged files with marker blocks in the Workspace view, and they are
//! written to the local directory for the user to resolve, as a FUSE mount
//! would show them.
use super::capture::{capture, CaptureError, Captured};
use super::ledger::{Ledger, Stored};
use super::mount::{Workspace, WorkspaceError};
use crate::provider::serve::basic_info;
use crate::provider::tree::{join, Found, Tree};
use crate::source::snapshot::SnapshotSource;
use crate::sys;
use crate::track::journal::Recorded;
use crate::track::local::{native, Local, LocalItem};
use crate::track::watch::Pending;
use layerfs_content::object::inode_leaf::InodeKind;
use layerfs_content::ObjectId;
use layerfs_history::CommitOutcome;
use layerfs_store_service::history::lookup::hex;
use layerfs_store_service::store::CommitOptions;
use layerfs_store_service::write::mutation::Mutation;
use std::sync::Arc;

/// What a Commit published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// A new Commit on the Branch.
    Committed { commit: String, root: String },
    /// Nothing differed from the Branch's head.
    UpToDate { head: Option<String>, root: String },
}

/// On-disk items brought to a new base.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Rebased {
    /// On-disk placeholders given the new base's facts and content.
    pub updated: u64,
    /// On-disk placeholders removed because the new base has no such file.
    pub removed: u64,
    /// Committed full files, unchanged since their Commit, that the new
    /// base holds differently or not at all: removed from the disk, so the
    /// new base shows through.
    pub replaced: u64,
    /// On-disk items ProjFS would not change, with its reason bits.
    pub refused: Vec<(String, i32)>,
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
    },
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

fn failed(error: impl std::fmt::Display) -> CommitRefusal {
    CommitRefusal::Failed(error.to_string())
}

/// Brings the on-disk items from `old` to `new`: every placeholder whose
/// base entry changed, and every committed full file that is still as it
/// was committed and that `new` holds differently. A full file changed
/// since its Commit is a local change and is left alone. Work follows the
/// items on disk, not the tree.
fn rebase(
    workspace: &Workspace,
    old: &Tree,
    new: &Tree,
    ledger: &mut Ledger,
) -> Result<Rebased, WorkspaceError> {
    let projection = &workspace.projection;
    let flags = sys::ALLOW_DIRTY_METADATA | sys::ALLOW_READ_ONLY;
    let mut rebased = Rebased::default();
    let _local = projection.provider().local_only();
    let mut local = Local::new(projection.root());
    let wrap = |e: &dyn std::fmt::Display| WorkspaceError::Failed(format!("baseline update: {e}"));
    // The record holds files of earlier Commits only: this Commit's are
    // added after this update. For each, the old base's entry is what this
    // Workspace committed.
    for (path, stored) in ledger.files() {
        let entry = |tree: &Tree| match tree.resolve(&path) {
            Ok(Some(Found::Entry { entry, .. })) => Ok(Some(entry)),
            Ok(_) => Ok(None),
            Err(error) => Err(wrap(&error)),
        };
        if entry(old)? == entry(new)? {
            continue;
        }
        // The Branch holds something else at this path now. What this
        // Workspace committed there is replaced unless it changed since.
        ledger.forget(&path);
        let unchanged = local
            .find(&path)
            .map_err(|e| wrap(&e))?
            .is_some_and(|(spelled, item)| {
                !item.directory && stored.is(&native(projection.root(), &spelled), &item)
            });
        if !unchanged {
            continue;
        }
        let every = flags | sys::ALLOW_DIRTY_DATA | sys::ALLOW_TOMBSTONE;
        match projection
            .instance()
            .delete_item(&path.replace('/', "\\"), every)
        {
            Ok(_) => rebased.replaced += 1,
            Err((_, cause)) => rebased.refused.push((path, cause)),
        }
    }
    for path in ledger.tombstones() {
        // A name this Workspace deleted is back in the base: its tombstone
        // would hide it.
        if new.resolve(&path).map_err(|e| wrap(&e))?.is_some()
            && projection
                .instance()
                .delete_item(&path.replace('/', "\\"), sys::ALLOW_TOMBSTONE)
                .is_ok()
        {
            ledger.cleared(&path);
        }
    }
    let mut pending = vec![String::new()];
    while let Some(directory) = pending.pop() {
        for item in local
            .list(&directory)
            .map_err(|e| wrap(&e))?
            .unwrap_or_default()
        {
            let path = join(&directory, &item.name);
            if item.directory {
                pending.push(path);
                continue;
            }
            let state = local.state(&path).map_err(|e| wrap(&e))?.unwrap_or(0);
            let placeholder = state & (sys::state::PLACEHOLDER | sys::state::HYDRATED) != 0
                && state & sys::state::FULL == 0;
            if !placeholder {
                continue;
            }
            let Some(Found::Entry { entry: was, .. }) = old.resolve(&path).map_err(|e| wrap(&e))?
            else {
                continue;
            };
            let name = path.replace('/', "\\");
            let done = match new.resolve(&path).map_err(|e| wrap(&e))? {
                Some(Found::Entry { entry: now, .. }) if !now.directory => {
                    if now == was {
                        continue;
                    }
                    projection
                        .instance()
                        .update_item(&name, &basic_info(&now), &now.content, flags)
                        .map(|changed| rebased.updated += u64::from(changed))
                }
                _ => projection
                    .instance()
                    .delete_item(&name, flags)
                    .map(|changed| rebased.removed += u64::from(changed)),
            };
            if let Err((_, cause)) = done {
                rebased.refused.push((path, cause));
            }
        }
    }
    Ok(rebased)
}

impl Workspace {
    pub(super) fn source_at(&self, root: ObjectId) -> Result<Arc<SnapshotSource>, WorkspaceError> {
        SnapshotSource::open(self.engine.store(), self.stack.scope, root)
            .map(Arc::new)
            .map_err(|e| WorkspaceError::Failed(format!("base tree: {e}")))
    }

    /// Serves the root the engine has now installed, updating on-disk
    /// placeholders when `merged` brought in changes this Workspace did
    /// not make.
    fn advance(&self, merged: bool, ledger: &mut Ledger) -> Result<Rebased, WorkspaceError> {
        let snapshot = self
            .bound
            .snapshot()
            .map_err(|e| WorkspaceError::Failed(format!("binding: {e}")))?;
        let source = self.source_at(snapshot.effective_root)?;
        let tree = &self.projection.provider().tree;
        // The new base is served first: a file first opened from now on is
        // projected from it, and what is already on disk is brought over
        // against the old one.
        let old = Tree::new(tree.source());
        tree.replace(source);
        let rebased = if merged {
            rebase(self, &old, tree, ledger)?
        } else {
            Rebased::default()
        };
        self.write_record()?;
        Ok(rebased)
    }

    /// Records what the published Commit stored for the files the capture
    /// read, and clears the tombstones of the names it removed.
    fn settle(&self, ledger: &mut Ledger, captured: &Captured) {
        let root = self.projection.root();
        for path in &captured.removed {
            ledger.forget(path);
            // The deletion is committed: the base no longer holds the name,
            // so the marker that hid it has nothing left to hide. Left on
            // disk it would hide the name if another Workspace brings it
            // back. Something else may be at the path by now; that stays.
            if std::fs::symlink_metadata(native(root, path)).is_ok() {
                continue;
            }
            let cleared = self
                .projection
                .instance()
                .delete_item(&path.replace('/', "\\"), sys::ALLOW_TOMBSTONE);
            if cleared.is_err() {
                ledger.tombstone(path);
            }
        }
        // No tree is read for this: the size, time and journal number a
        // file had when the capture read it are the whole record.
        for seen in &captured.files {
            ledger.record(
                &seen.path,
                Stored {
                    size: seen.size,
                    written: seen.written,
                    change: seen.change,
                },
            );
        }
    }

    /// Writes the view's present content of `paths` into the directory.
    fn materialize(&self, paths: &[String]) -> Result<(), CommitRefusal> {
        let mutation = Mutation::new(&self.bound).map_err(|r| failed(r.message))?;
        let result = (|| {
            for path in paths {
                // Written by this process; the volume's change journal
                // marks them like any other change.
                let relative = path.trim_start_matches('/');
                let local = native(self.projection.root(), relative);
                match mutation.optional(path).map_err(|r| failed(r.message))? {
                    Some(stat) if stat.kind == InodeKind::RegularFile => {
                        if let Some(parent) = local.parent() {
                            std::fs::create_dir_all(parent).map_err(failed)?;
                        }
                        let mut bytes = Vec::with_capacity(stat.logical_len as usize);
                        while (bytes.len() as u64) < stat.logical_len {
                            let piece = mutation
                                .read(stat.serial, bytes.len() as u64, 32 * 1024)
                                .map_err(|r| failed(r.message))?;
                            if piece.is_empty() {
                                return Err(failed(format!("{path} ended early")));
                            }
                            bytes.extend_from_slice(&piece);
                        }
                        std::fs::write(&local, bytes).map_err(failed)?;
                    }
                    Some(stat) if stat.kind == InodeKind::Directory => {
                        std::fs::create_dir_all(&local).map_err(failed)?;
                    }
                    Some(_) => {}
                    None => match std::fs::remove_file(&local) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(failed(e)),
                    },
                }
            }
            Ok(())
        })();
        mutation.finish().map_err(|r| failed(r.message))?;
        result
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
        let recorded = self.projection.provider().journal.recorded();
        // Everything before `position` in the change journal is in
        // `pending`; what comes after stays with the tracker.
        let (pending, position) = self.projection.watch().take();
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut published = self.publish(message, &recorded, &pending, &mut ledger);
        match &mut published {
            // Published: a failure from here on does not undo it, and is
            // told beside it instead of in its place.
            Ok(committed) => {
                if let Err(error) = self.projection.settle(position) {
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
            Err(_) => self.projection.watch().restore(pending),
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
        // The engine installed a root: the Commit's, or a merge's.
        let rebased = match self.advance(merged, ledger) {
            Ok(rebased) => rebased,
            Err(error) if success.history.is_some() => {
                warnings.push(format!(
                    "the directory was not brought to the new base ({error}): \
                     unmount and resume before the next Commit"
                ));
                Rebased::default()
            }
            Err(error) => return Err(failed(error)),
        };
        let Some(history) = success.history else {
            let merge = success.merge.unwrap_or_else(unreachable_merge);
            self.materialize(&merge.written)?;
            return Err(CommitRefusal::Conflict {
                head: merge.head.map(|id| hex(id.as_slice())),
                conflicts: merge.conflicts.iter().map(|c| c.path.clone()).collect(),
                written: merge.written,
            });
        };
        // Published. A file still open for writing is compared again at
        // the next Commit: the tracker keeps it until the journal shows
        // its handle closed.
        self.settle(ledger, &captured);
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
