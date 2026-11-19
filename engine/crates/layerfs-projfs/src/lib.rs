//! Reference prototype for #454: a LayerFS Workspace on native Windows,
//! mounted through the Projected File System (ProjFS). No WSL2, Docker,
//! WinFsp or driver of its own. See `REPORT.md` beside this crate.
//!
//! Windows only: on every other platform this library is empty.
#![cfg(windows)]
#![deny(unsafe_code)]

pub mod cli;
pub mod ntfs;
pub mod projection;
pub mod provider;
pub mod source;
pub mod store;
pub mod sys;
pub mod track;
pub mod workspace;

pub use projection::{MountError, Projection};
pub use track::{Change, Kind};
pub use workspace::{From, MountRequest, Workspace};
