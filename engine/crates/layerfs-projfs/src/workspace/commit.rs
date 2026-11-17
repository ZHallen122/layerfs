//! Commit: capture the journaled changes into the bound Workspace, publish
//! through the engine's existing Commit, then move the projection's base to
//! the root the engine installed.
//!
//! Publication, merge with a moved Branch head and Conflict detection are
//! the engine's (`BoundWorkspace::commit_captured`); nothing here decides
//! them. A conflicted merge publishes nothing: the engine installs the
//! merged files with marker blocks in the Workspace view, and they are
//! written to the local directory for the user to resolve, as a FUSE mount
//! would show them.
use super::capture::{capture, CaptureError, Captured};
use super::mount::{Workspace, WorkspaceError};
use crate::provider::serve::basic_info;
use crate::provider::tree::{join, Found, Tree};
use crate::source::snapshot::SnapshotSource;
use crate::sys;
use crate::track::local::{native, Local};
use layerfs_content::object::inode_leaf::InodeKind;
use layerfs_content::ObjectId;
use layerfs_history::CommitOutcome;
use layerfs_store_service::history::lookup::{hex, unhex};
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

/// Placeholders brought to a new base.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Rebased {
    /// On-disk placeholders given the new base's facts and content.
    pub updated: u64,
    /// On-disk placeholders removed because the new base has no such file.
    pub removed: u64,
    /// On-disk placeholders ProjFS would not change, with its reason bits.
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
            Self::Unknown(text) => write!(f, "Unknown: {text}"),
        }
    }
}
impl std::error::Error for CommitRefusal {}

fn failed(error: impl std::fmt::Display) -> CommitRefusal {
    CommitRefusal::Failed(error.to_string())
}

/// Brings the on-disk placeholders from `old` to `new`. A file that is
/// full on disk is a local change and is left alone. Work follows the
/// items on disk, not the tree.
fn rebase(workspace: &Workspace, old: &Tree, new: &Tree) -> Result<Rebased, WorkspaceError> {
    let projection = &workspace.projection;
    let flags = sys::ALLOW_DIRTY_METADATA | sys::ALLOW_READ_ONLY;
    let mut rebased = Rebased::default();
    let _local = projection.provider().local_only();
    let mut local = Local::new(projection.root());
    let wrap = |e: &dyn std::fmt::Display| WorkspaceError::Failed(format!("baseline update: {e}"));
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
    fn source_at(&self, root: ObjectId) -> Result<Arc<SnapshotSource>, WorkspaceError> {
        SnapshotSource::open(self.engine.store(), self.stack.scope, root)
            .map(Arc::new)
            .map_err(|e| WorkspaceError::Failed(format!("base tree: {e}")))
    }

    /// Serves the root the engine has now installed, updating on-disk
    /// placeholders when `merged` brought in changes this Workspace did
    /// not make.
    fn advance(&self, merged: bool) -> Result<Rebased, WorkspaceError> {
        let snapshot = self
            .bound
            .snapshot()
            .map_err(|e| WorkspaceError::Failed(format!("binding: {e}")))?;
        let source = self.source_at(snapshot.effective_root)?;
        let tree = &self.projection.provider().tree;
        let rebased = if merged {
            rebase(self, tree, &Tree::new(source.clone()))?
        } else {
            Rebased::default()
        };
        tree.replace(source);
        self.write_record()?;
        Ok(rebased)
    }

    /// Updates on-disk placeholders projected from the tree `root` (hex)
    /// to the base this Workspace is bound to now.
    pub(super) fn rebase_from(&self, root: &str) -> Result<Rebased, WorkspaceError> {
        let old = unhex(root)
            .and_then(|bytes| ObjectId::from_bytes(&bytes).ok())
            .ok_or_else(|| WorkspaceError::Failed("recorded base root".to_owned()))?;
        let old = Tree::new(self.source_at(old)?);
        rebase(self, &old, &self.projection.provider().tree)
    }

    /// Writes the view's present content of `paths` into the directory.
    fn materialize(&self, paths: &[String]) -> Result<(), CommitRefusal> {
        let mutation = Mutation::new(&self.bound).map_err(|r| failed(r.message))?;
        let result = (|| {
            for path in paths {
                let relative = path.trim_start_matches('/');
                // ProjFS reports nothing the provider's own process does.
                self.projection.provider().journal.touch(relative);
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
    /// engine Commit, no retry. Changes reported after the capture began
    /// stay journaled for the next Commit.
    pub fn commit(&self, message: Option<&str>) -> Result<Committed, CommitRefusal> {
        let _one = self
            .committing
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let provider = self.projection.provider();
        self.projection
            .sweep()
            .map_err(|e| CommitRefusal::Failed(format!("attribute sweep: {e}")))?;
        let recorded = provider.journal.recorded();
        let captured = capture(&self.bound, provider, self.projection.root(), &recorded)
            .map_err(CommitRefusal::Capture)?;
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
        // The engine installed a root: the Commit's, or a merge's.
        let rebased = self.advance(merged).map_err(failed)?;
        let Some(history) = success.history else {
            let merge = success.merge.unwrap_or_else(unreachable_merge);
            self.materialize(&merge.written)?;
            return Err(CommitRefusal::Conflict {
                head: merge.head.map(|id| hex(id.as_slice())),
                conflicts: merge.conflicts.iter().map(|c| c.path.clone()).collect(),
                written: merge.written,
            });
        };
        // Published. Files still open for writing elsewhere stay journaled.
        let live: Vec<String> = captured
            .live
            .iter()
            .map(|path| path.to_lowercase())
            .collect();
        provider
            .journal
            .forget_through(recorded.sequence, &|path| {
                live.contains(&path.to_lowercase())
            })
            .map_err(failed)?;
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
