//! The Store half of a mount, with no ProjFS involved: Project Init of a
//! native directory, and the snapshot source the provider would serve lists
//! and reads exactly that tree.
#![cfg(windows)]

mod support;

use layerfs_projfs::source::snapshot::SnapshotSource;
use layerfs_projfs::source::Source;
use layerfs_projfs::store::{init_project, Engine};
use std::collections::BTreeMap;
use std::os::windows::fs::MetadataExt;

/// Every entry below `directory` of the source: path to (is directory,
/// size, last write ticks).
fn listed(source: &dyn Source, directory: &str, into: &mut BTreeMap<String, (bool, u64, i64)>) {
    for entry in source.list(directory).expect("list").entries {
        let path = if directory.is_empty() {
            entry.name.clone()
        } else {
            format!("{directory}/{}", entry.name)
        };
        if entry.directory {
            listed(source, &path, into);
        }
        // A directory's time is the Store's own; only files carry theirs.
        let written = if entry.directory { 0 } else { entry.written };
        into.insert(path, (entry.directory, entry.size, written));
    }
}

#[test]
fn an_initialized_directory_is_listed_and_read_back_exactly() {
    let home = support::Scratch::new("store-snapshot");
    let tree = home.path().join("tree");
    let files = support::write_tree(&tree);
    let store = home.path().join("store.sqlite");
    init_project(&store, "demo", "work", &tree).expect("Project Init");
    let engine = Engine::open(&store, &home.path().join("overlay")).expect("engine");
    let stack = engine.project("demo").expect("stack");
    let branch = engine.branch(&stack, "work").expect("branch");
    let workspace = engine.bind(&branch).expect("bind");
    let snapshot = workspace.snapshot().expect("snapshot");
    let source = SnapshotSource::open(engine.store(), snapshot.scope, snapshot.effective_root)
        .expect("source");

    let mut seen = BTreeMap::new();
    listed(&source, "", &mut seen);
    let mut expected = BTreeMap::new();
    for (path, _) in &files {
        let native = path.split('/').fold(tree.clone(), |at, name| at.join(name));
        let metadata = std::fs::metadata(&native).expect("metadata");
        expected.insert(
            path.clone(),
            (
                false,
                metadata.file_size(),
                metadata.last_write_time() as i64,
            ),
        );
        let mut parent = path.as_str();
        while let Some((above, _)) = parent.rsplit_once('/') {
            expected.insert(above.to_owned(), (true, 0, 0));
            parent = above;
        }
    }
    assert_eq!(seen, expected, "names, sizes and file times");

    // Every file's bytes, read through the source in two windows.
    for (path, bytes) in &files {
        let (directory, name) = path.rsplit_once('/').unwrap_or(("", path));
        let entry = source
            .list(directory)
            .expect("list")
            .entries
            .into_iter()
            .find(|entry| entry.name == name)
            .expect("entry");
        let mut read = vec![0u8; bytes.len()];
        let half = bytes.len() / 2;
        source
            .read(path, &entry.content, 0, &mut read[..half])
            .expect("first half");
        source
            .read(path, &entry.content, half as u64, &mut read[half..])
            .expect("second half");
        assert_eq!(&read, bytes, "{path}");
    }
    engine.release(&workspace).expect("release");
    engine.close().expect("close");
}
