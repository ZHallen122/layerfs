//! The prototype's entry point: a LayerFS Workspace as a local directory.
pub mod capture;
pub mod commit;
pub mod follow;
pub mod ledger;
pub mod mount;
pub mod record;
pub mod unfollowed;

pub use capture::{CaptureError, Captured};
pub use commit::{CommitRefusal, Committed, Outcome};
pub use follow::Rebased;
pub use mount::{default_state, From, MountRequest, Status, Workspace, WorkspaceError};
