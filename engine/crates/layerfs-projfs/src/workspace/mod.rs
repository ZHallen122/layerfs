//! The prototype's entry point: a LayerFS Workspace as a local directory.
pub mod capture;
pub mod commit;
pub mod mount;
pub mod record;

pub use capture::{CaptureError, Captured};
pub use commit::{CommitRefusal, Committed, Outcome, Rebased};
pub use mount::{default_state, From, MountRequest, Status, Workspace, WorkspaceError};
