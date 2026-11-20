//! The ProjFS provider: projects a source's tree and records changes.
pub mod serve;
pub mod tree;

pub use serve::{LocalOnly, Provider, Served};
pub use tree::{Found, Tree};
