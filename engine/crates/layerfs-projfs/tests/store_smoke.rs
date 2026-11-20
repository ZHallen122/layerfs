//! The existing Store engine on native Windows, with no ProjFS involved:
//! Project Init of a directory, a Commit through a bound Workspace, and the
//! Commit read back.
#![cfg(windows)]

mod support;

use layerfs_projfs::store::{init_project, Engine};
use layerfs_store_service::read::snapshot;
use layerfs_store_service::store::CommitOptions;
use layerfs_store_service::write::mutation::Mutation;

#[test]
fn a_workspace_commit_is_read_back_from_the_store() {
    let home = support::Scratch::new("store-smoke");
    let tree = home.path().join("tree");
    support::write_tree(&tree);
    let store = home.path().join("store.sqlite");
    let initialized = init_project(&store, "demo", "work", &tree).expect("Project Init");
    assert!(initialized.entries > 8, "{initialized:?}");

    let engine = Engine::open(&store, &home.path().join("overlay")).expect("engine");
    let stack = engine.project("demo").expect("stack");
    let branch = engine.branch(&stack, "work").expect("branch");
    assert_eq!(branch.head_commit, None, "a new Branch has no Commit");

    let workspace = engine.bind(&branch).expect("bind");
    let mut mutation = Mutation::new(&workspace).expect("mutation");
    mutation.mkdir("/added", None).expect("mkdir");
    mutation
        .write("/added/hello.txt", b"hello from Windows\n", None)
        .expect("write");
    mutation.finish().expect("finish");
    let committed = workspace
        .commit_captured(&CommitOptions {
            force: false,
            message: Some("smoke".to_owned()),
        })
        .expect("commit");
    assert!(committed.history.is_some(), "the Commit is published");
    engine.release(&workspace).expect("release");

    let branch = engine.branch(&stack, "work").expect("branch");
    assert!(branch.head_commit.is_some(), "the Branch head moved");
    let (reader, snap) = snapshot::open(engine.store(), &stack, "work").expect("snapshot");
    let entry = snapshot::existing(reader.as_ref(), &snap, "/added/hello.txt").expect("entry");
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
    assert_eq!(bytes, b"hello from Windows\n");
    // main, the project's line of Layers, is untouched by a Branch Commit.
    let (reader, main) = snapshot::open(engine.store(), &stack, "main").expect("main");
    assert!(snapshot::find(reader.as_ref(), &main.tree, "/added")
        .expect("lookup")
        .is_none());
    assert!(snapshot::find(reader.as_ref(), &main.tree, "/README.md")
        .expect("lookup")
        .is_some());
    drop(reader);
    engine.close().expect("close");
}

#[test]
fn a_store_whose_access_was_widened_is_refused() {
    let home = support::Scratch::new("store-acl");
    let tree = home.path().join("tree");
    support::write_tree(&tree);
    let store = home.path().join("store.sqlite");
    init_project(&store, "demo", "work", &tree).expect("Project Init");
    // The direct Store route opens only a Store file restricted to its owner.
    let granted = std::process::Command::new("icacls")
        .arg(&store)
        .args(["/grant", "*S-1-1-0:(R)"])
        .output()
        .expect("icacls");
    assert!(granted.status.success());
    let refused = Engine::open(&store, &home.path().join("overlay"))
        .err()
        .expect("a widened Store is refused");
    assert!(refused.to_string().starts_with("Unsupported:"), "{refused}");
}
