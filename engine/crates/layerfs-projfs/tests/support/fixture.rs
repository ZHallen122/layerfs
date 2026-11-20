//! A Store with one project whose Branch `work` holds the fixture tree,
//! and the mounts and Store reads the Workspace tests share.
use super::Scratch;
use layerfs_history::LayerStackRecord;
use layerfs_projfs::store::{init_project, Engine};
use layerfs_projfs::{From, MountRequest, Workspace};
use layerfs_store_service::read::{snapshot, walk};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A saved tree: path to (is directory, size), and every file's digest.
pub type Saved = (BTreeMap<String, (bool, u64)>, BTreeMap<String, [u8; 32]>);

pub struct Fixture {
    pub home: Scratch,
    pub engine: Arc<Engine>,
    pub stack: LayerStackRecord,
    /// The imported tree's files: path to bytes.
    pub files: Vec<(String, Vec<u8>)>,
    /// The Layer Project Init published: the tree every Branch starts from.
    pub imported: String,
}

impl Fixture {
    pub fn new(label: &str) -> Self {
        let home = Scratch::new(label);
        let tree = home.path().join("tree");
        let files = super::write_tree(&tree);
        let store = home.path().join("store.sqlite");
        init_project(&store, "demo", "work", &tree).expect("Project Init");
        let engine = Arc::new(Engine::open(&store, &home.path().join("overlay")).expect("engine"));
        let stack = engine.project("demo").expect("stack");
        let imported = layerfs_store_service::history::lookup::node_text(
            layerfs_history::NodeId::Layer(stack.head_layer),
        );
        Self {
            home,
            engine,
            stack,
            files,
            imported,
        }
    }
    pub fn root(&self, name: &str) -> PathBuf {
        self.home.path().join(name)
    }
    pub fn mount(&self, name: &str) -> Workspace {
        self.mount_from(name, From::Branch("work"))
    }
    pub fn mount_from(&self, name: &str, from: From<'_>) -> Workspace {
        Workspace::mount(
            &self.engine,
            MountRequest {
                project: "demo",
                from,
                root: &self.root(name),
                state: None,
                threads: 0,
            },
        )
        .expect("mount")
    }
    pub fn head(&self) -> Option<String> {
        self.engine
            .branch(&self.stack, "work")
            .expect("branch")
            .head_commit
            .map(|id| layerfs_store_service::history::lookup::hex(id.as_slice()))
    }
    /// The tree saved at `name` (a Branch or a Commit identity), read
    /// through the Store's read calls: path to (is directory, size), and
    /// every file's digest.
    pub fn saved(&self, name: &str) -> Saved {
        let (reader, snap) = snapshot::open(self.engine.store(), &self.stack, name).expect("open");
        let mut entries = Vec::new();
        walk::walk(&reader, &snap, "/", |path, entry| {
            entries.push((path.trim_start_matches('/').to_owned(), entry));
            Ok(true)
        })
        .expect("walk");
        let (mut shape, mut digests) = (BTreeMap::new(), BTreeMap::new());
        for (path, entry) in entries {
            use layerfs_content::object::inode_leaf::InodeKind;
            if entry.value.kind == InodeKind::Directory {
                shape.insert(path, (true, 0));
                continue;
            }
            let mut bytes = Vec::new();
            layerfs_store_service::telemetry::timed("read", |timing| {
                layerfs_content::file::read_all(
                    reader.as_ref(),
                    entry.value.content_root,
                    &mut bytes,
                    timing.child("content.read"),
                )
            })
            .expect("read");
            shape.insert(path.clone(), (false, bytes.len() as u64));
            digests.insert(path, *blake3::hash(&bytes).as_bytes());
        }
        (shape, digests)
    }
    pub fn close(self) {
        let Self { home, engine, .. } = self;
        Arc::try_unwrap(engine)
            .ok()
            .expect("no Workspace still holds the engine")
            .close()
            .expect("close");
        drop(home);
    }
}

/// A mounted directory's shape as the Store reports one: (is directory, size).
pub fn shape(root: &Path) -> BTreeMap<String, (bool, u64)> {
    super::tree_state(root)
        .into_iter()
        .map(|(path, (directory, size, _))| (path, (directory, size)))
        .collect()
}
