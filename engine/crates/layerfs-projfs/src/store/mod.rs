//! The existing global Store, opened in this process through the
//! Windows-native Store route (#452): Project Init of a native tree, and the
//! engine session Workspaces bind through. Nothing here is a second Store,
//! encoder or history: every call is an existing port.
pub mod engine;
pub mod keys;
pub mod project;

pub use engine::{Engine, EngineError};
pub use keys::{BINDING, CURSOR_KEY};
pub use project::{init_project, Initialized};
