//! One Store opened in this process through the direct Store route, with
//! the overlay owner its Workspaces construct through.
use super::{BINDING, CURSOR_KEY};
use layerfs_history::{BranchRecord, LayerStackRecord, WorkspaceId};
use layerfs_store_service::history::lookup;
use layerfs_store_service::store::{BoundWorkspace, Store};
use layerfs_store_service::{OpenRefusal, StoreService};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Why an engine call was refused. The text keeps the SDK's refusal code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineError {
    /// The caller's input is not valid.
    Invalid(String),
    /// A named project, Branch, Commit or path does not exist.
    NotFound(String),
    /// The direct Store route refuses this setup, with its reason.
    Unsupported(String),
    /// The Store or overlay refused or failed; the text is its own cause.
    Failed(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(text) => write!(f, "InvalidInput: {text}"),
            Self::NotFound(text) => write!(f, "NotFound: {text}"),
            Self::Unsupported(text) => write!(f, "Unsupported: {text}"),
            Self::Failed(text) => write!(f, "Failed: {text}"),
        }
    }
}
impl std::error::Error for EngineError {}

impl From<layerfs_bridge::control::SdkRefusal> for EngineError {
    fn from(refusal: layerfs_bridge::control::SdkRefusal) -> Self {
        let text = format!("{}: {}", refusal.code, refusal.message);
        match refusal.code.as_str() {
            "InvalidInput" => Self::Invalid(text),
            "NoSuchProject" | "NoSuchBranch" | "NoSuchCommit" | "NotFound" | "NoSuchPath" => {
                Self::NotFound(text)
            }
            _ => Self::Failed(text),
        }
    }
}

/// The Store opened in this process, with the overlay template its
/// Workspaces copy. One per process per Store file.
pub struct Engine {
    service: StoreService,
    scratch: PathBuf,
}

impl Engine {
    /// Opens the Store file `store` through `StoreService::open_direct`: the
    /// kernel guard (a local NTFS volume), the owner-only check of the Store
    /// file, the Store selected Disposable, and the overlay template under
    /// `scratch` (a local directory created here and removed by
    /// [`Self::close`]).
    pub fn open(store: &Path, scratch: &Path) -> Result<Self, EngineError> {
        std::fs::create_dir_all(scratch)
            .map_err(|e| EngineError::Failed(format!("overlay directory: {e}")))?;
        let service = StoreService::open_direct(
            store,
            BINDING,
            CURSOR_KEY,
            &scratch.join("overlay-template.sqlite"),
        )
        .map_err(|refusal| match refusal {
            OpenRefusal::Unsupported(reason) => EngineError::Unsupported(reason.0),
            OpenRefusal::Missing(text) => EngineError::NotFound(text),
            OpenRefusal::Failed(text) => EngineError::Failed(text),
        })?;
        Ok(Self {
            service,
            scratch: scratch.to_owned(),
        })
    }

    /// The opened Store.
    pub fn store(&self) -> &Arc<Store> {
        &self.service.store
    }

    /// The Store service whose private Workspaces the mounts hold.
    pub fn service(&self) -> &StoreService {
        &self.service
    }

    /// The project's stack, by name.
    pub fn project(&self, name: &str) -> Result<LayerStackRecord, EngineError> {
        Ok(lookup::project(self.store(), name)?)
    }

    /// One Branch of `stack`, by name.
    pub fn branch(
        &self,
        stack: &LayerStackRecord,
        name: &str,
    ) -> Result<BranchRecord, EngineError> {
        Ok(lookup::branch(self.store(), stack, name)?)
    }

    /// Binds one Workspace on `branch` at its present head.
    pub fn bind(&self, branch: &BranchRecord) -> Result<Arc<BoundWorkspace>, EngineError> {
        let id = layerfs_bridge::duplex::generate_keypair()
            .map_err(|e| EngineError::Failed(format!("randomness: {e}")))
            .and_then(|pair| {
                WorkspaceId::from_authority(pair.public)
                    .map_err(|e| EngineError::Failed(format!("Workspace identity: {e}")))
            })?;
        layerfs_store_service::telemetry::timed("projfs.bind", |timing| {
            self.service.bind(id, branch.id, timing)
        })
        .map_err(EngineError::Failed)
    }

    /// Closes `workspace` and retires its overlay file.
    pub fn release(&self, workspace: &BoundWorkspace) -> Result<(), EngineError> {
        self.service.release(workspace).map_err(EngineError::Failed)
    }

    /// Closes the Store and its overlay owner, then removes the scratch
    /// directory.
    pub fn close(self) -> Result<(), EngineError> {
        let Self { service, scratch } = self;
        service.close().map_err(EngineError::Failed)?;
        match std::fs::remove_dir_all(&scratch) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(EngineError::Failed(format!("overlay directory: {error}"))),
        }
    }
}
