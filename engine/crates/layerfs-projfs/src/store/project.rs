//! A new Store from a native Windows tree, through the existing Project
//! Init (`layerfs-project`, native on Windows since #452). Nothing is
//! imported here by another route.
use super::{EngineError, BINDING, CURSOR_KEY};
use layerfs_history::{BranchId, HistoryCatalogConfig, HistoryName, LayerStackId};
use layerfs_persistence::{PersistenceConfig, SqlitePersistenceProfile};
use layerfs_project::store::{InitRequest, ProjectApi};
use layerfs_storage::StoragePolicy;
use layerfs_store_service::history::lookup::hex;
use std::path::Path;
use std::time::{Duration, Instant};

/// Where the Store sits in a daemon's filesystem after installation. This
/// route installs nowhere; Init only checks the text's shape.
const LOCATOR: &str = "/layerfs-store/global/store.sqlite";
/// The Init deadline. It bounds Init only, never a later Workspace.
const DEADLINE: Duration = Duration::from_secs(60 * 60);

/// What an Init acknowledged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Initialized {
    /// Source entries imported, the root directory included.
    pub entries: u64,
    /// The sealed Store file's length.
    pub store_bytes: u64,
    /// Hex identity of the imported tree's root.
    pub root: String,
}

fn random() -> Result<[u8; 32], EngineError> {
    layerfs_bridge::duplex::generate_keypair()
        .map(|pair| pair.public)
        .map_err(|error| EngineError::Failed(format!("randomness: {error}")))
}

/// Creates the Store file `store` from the complete directory `source`:
/// project `project`, whose main line is that tree, and its first working
/// Branch `branch`. The Store is created Disposable, sealed, and restricted
/// to its owner, as the direct Store route requires of a Store it opens.
pub fn init_project(
    store: &Path,
    project: &str,
    branch: &str,
    source: &Path,
) -> Result<Initialized, EngineError> {
    let name = |text: &str| {
        HistoryName::new(text).map_err(|e| EngineError::Invalid(format!("{text}: {e}")))
    };
    let (stack_name, branch_name) = (name(project)?, name(branch)?);
    let mut body = [0; 16];
    body.copy_from_slice(&random()?[..16]);
    let stack = LayerStackId::from_authority(body);
    let request = InitRequest {
        source: source.to_owned(),
        store: PersistenceConfig::sqlite(store)
            .with_sqlite_profile(SqlitePersistenceProfile::Disposable),
        locator: LOCATOR.to_owned(),
        policy: StoragePolicy::frozen_default(),
        catalog: HistoryCatalogConfig {
            binding_key: BINDING.to_vec(),
            cursor_key: CURSOR_KEY,
            incarnation: 1,
        },
        stack,
        stack_name,
        branch: BranchId::named(stack, &branch_name),
        branch_name,
        scope_seed: random()?,
        deadline: Instant::now() + DEADLINE,
    };
    let sealed = layerfs_store_service::telemetry::timed("projfs.init", |timing| {
        ProjectApi::new().init(request, timing)
    })
    .map_err(|failure| EngineError::Failed(failure.to_string()))?;
    layerfs_store_service::windows_acl::restrict_to_owner(&sealed.store.path)
        .map_err(|e| EngineError::Failed(format!("Store access control: {e}")))?;
    Ok(Initialized {
        entries: sealed.initialized.entries,
        store_bytes: sealed.store.bytes,
        root: hex(sealed.initialized.root.as_bytes()),
    })
}
