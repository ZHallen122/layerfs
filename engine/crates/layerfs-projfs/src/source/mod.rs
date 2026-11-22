//! What a mount projects: an immutable tree read on demand.
//!
//! Paths are relative to the tree's root, separated by `/`, in the source's
//! own spelling; the root is the empty path. The provider resolves the
//! spelling an application typed (any case) to the source's before it asks.
pub mod attribute;
pub mod directory;
pub mod snapshot;
pub mod types;

pub use types::{ticks, unix, Entry, Listing, Source, SourceError, EPOCH_TICKS};
