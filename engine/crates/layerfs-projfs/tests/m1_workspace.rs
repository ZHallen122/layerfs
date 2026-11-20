//! M1: a Workspace backed by the LayerFS Store. A committed snapshot is
//! projected, edited by another process, and committed through the existing
//! Commit path; the result is read back through the Store's own read calls.
#![cfg(windows)]

mod support;

use layerfs_projfs::workspace::{CommitRefusal, Outcome};
use layerfs_projfs::{From, Kind, Workspace};
use std::path::Path;
use support::fixture::{shape, Fixture};
use support::ops::{self, Op};
use support::{at, tree_digests, tree_state};

#[test]
fn child_process_entry() {
    ops::child_entry();
}

#[test]
fn the_mounted_tree_equals_the_commit() {
    let fixture = Fixture::new("m1-equal");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    let (saved_shape, saved_digests) = fixture.saved("work");
    assert_eq!(shape(&root), saved_shape, "names and sizes");
    assert_eq!(tree_digests(&root), saved_digests, "content");
    // Supported metadata: file times and the read-only attribute.
    let original = tree_state(&fixture.home.path().join("tree"));
    assert_eq!(tree_state(&root), original, "file times");
    assert!(std::fs::metadata(at(&root, "locked/read-only.txt"))
        .expect("metadata")
        .permissions()
        .readonly());
    assert!(!std::fs::metadata(at(&root, "README.md"))
        .expect("metadata")
        .permissions()
        .readonly());
    let status = workspace.status().expect("status");
    assert_eq!(status.changes, Vec::new());
    assert_eq!(
        status.head, None,
        "a Branch fresh from Init has no Commit yet"
    );
    workspace.discard().expect("discard");
    fixture.close();
}

fn edit(fixture: &Fixture, root: &Path) {
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(root.join("new.txt"), b"new file\n".to_vec()),
            Op::Mkdir(at(root, "made/deeper")),
            Op::Write(
                at(root, "made/deeper/inside.txt"),
                support::pattern(9, 70_000),
            ),
            Op::Write(
                at(root, "src/main.rs"),
                b"fn main() { edited(); }\n".to_vec(),
            ),
            Op::Append(at(root, "assets/large.bin"), b"appended".to_vec()),
            Op::Remove(root.join("empty.bin")),
            Op::RemoveTree(root.join("docs")),
            Op::Rename(at(root, "README.md"), at(root, "README.txt")),
        ],
    );
}

#[test]
fn edits_commit_to_the_branch_and_read_back_exactly() {
    let fixture = Fixture::new("m1-commit");
    let before = fixture.saved("work");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    edit(&fixture, &root);
    let changes = workspace.status().expect("status").changes;
    assert!(changes.iter().any(|c| c.path == "README.txt"
        && c.kind
            == Kind::Renamed {
                from: "README.md".to_owned()
            }));
    assert_eq!(changes.len(), 9, "{changes:?}");

    let committed = workspace.commit(Some("edits")).expect("commit");
    let Outcome::Committed { commit, .. } = &committed.outcome else {
        panic!("expected a new Commit: {committed:?}");
    };
    assert!(!committed.merged);
    assert_eq!(
        fixture.head().as_deref(),
        Some(commit.as_str()),
        "Branch head"
    );
    assert_ne!(commit, &fixture.imported);

    // The new Commit, read through the Store, is exactly the edited tree.
    let (saved_shape, saved_digests) = fixture.saved("work");
    assert_eq!(saved_shape, shape(&root));
    assert_eq!(saved_digests, tree_digests(&root));
    assert!(saved_shape.contains_key("made/deeper/inside.txt"));
    assert!(!saved_shape.contains_key("docs"));
    assert!(!saved_shape.contains_key("README.md"));
    // The old Commit is unchanged.
    assert_eq!(fixture.saved(&fixture.imported), before);
    // Nothing is left to commit, and a second Commit says so.
    assert_eq!(workspace.status().expect("status").changes, Vec::new());
    let again = workspace.commit(None).expect("second commit");
    assert!(
        matches!(again.outcome, Outcome::UpToDate { .. }),
        "{again:?}"
    );

    // A fresh mount of the Branch shows the edited tree, times included.
    let edited = tree_state(&root);
    let fresh = fixture.mount("fresh");
    assert_eq!(tree_state(fresh.root()), edited);
    assert_eq!(tree_digests(fresh.root()), saved_digests);
    fresh.discard().expect("discard");
    workspace.discard().expect("discard");
    fixture.close();
}

#[test]
fn uncommitted_changes_of_one_workspace_are_invisible_in_another() {
    let fixture = Fixture::new("m1-isolated");
    let (first, second) = (fixture.mount("a"), fixture.mount("b"));
    let original = tree_state(second.root());
    let digests = tree_digests(second.root());
    edit(&fixture, first.root());
    assert_eq!(tree_state(second.root()), original);
    assert_eq!(tree_digests(second.root()), digests);
    assert_eq!(second.status().expect("status").changes, Vec::new());
    // And the other way round.
    ops::run(
        fixture.home.path(),
        &[Op::Write(second.root().join("only-b.txt"), b"b\n".to_vec())],
    );
    assert!(!first.root().join("only-b.txt").exists());
    first.discard().expect("discard");
    second.discard().expect("discard");
    fixture.close();
}

#[test]
fn two_commits_changing_one_file_keep_the_existing_conflict_behavior() {
    let fixture = Fixture::new("m1-conflict");
    let (first, second) = (fixture.mount("a"), fixture.mount("b"));
    let file = "src/main.rs";
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(at(first.root(), file), b"fn main() { first(); }\n".to_vec()),
            Op::Write(
                at(second.root(), file),
                b"fn main() { second(); }\n".to_vec(),
            ),
        ],
    );
    let won = first.commit(Some("first")).expect("first commit");
    let Outcome::Committed { commit, .. } = won.outcome else {
        panic!("first commit: {won:?}");
    };
    let refused = second
        .commit(Some("second"))
        .expect_err("the second Commit conflicts");
    let CommitRefusal::Conflict {
        conflicts, head, ..
    } = &refused
    else {
        panic!("expected a Conflict: {refused:?}");
    };
    assert_eq!(conflicts, &vec![format!("/{file}")]);
    assert_eq!(head.as_deref(), Some(commit.as_str()));
    // Nothing was published: the Branch still holds the first Commit.
    assert_eq!(fixture.head().as_deref(), Some(commit.as_str()));
    // The conflicted file holds marker blocks in the directory.
    let text = std::fs::read_to_string(at(second.root(), file)).expect("read");
    assert!(
        text.contains("<<<<<<<") && text.contains(">>>>>>>"),
        "{text}"
    );
    assert!(
        text.contains("first()") && text.contains("second()"),
        "{text}"
    );
    // Resolving the markers and committing again publishes.
    ops::run(
        fixture.home.path(),
        &[Op::Write(
            at(second.root(), file),
            b"fn main() { both(); }\n".to_vec(),
        )],
    );
    let resolved = second.commit(Some("resolved")).expect("resolved commit");
    assert!(
        matches!(resolved.outcome, Outcome::Committed { .. }),
        "{resolved:?}"
    );
    let (_, digests) = fixture.saved("work");
    assert_eq!(
        digests[file],
        *blake3::hash(b"fn main() { both(); }\n").as_bytes()
    );
    first.discard().expect("discard");
    second.discard().expect("discard");
    fixture.close();
}

#[test]
fn a_commit_over_a_moved_head_merges_and_updates_the_baseline() {
    let fixture = Fixture::new("m1-merge");
    let (first, second) = (fixture.mount("a"), fixture.mount("b"));
    // The second Workspace has README.md on disk as a hydrated placeholder
    // and never touches empty.bin.
    let _ = std::fs::read(second.root().join("README.md")).expect("read");
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(
                first.root().join("README.md"),
                b"# changed by first\n".to_vec(),
            ),
            Op::Write(first.root().join("from-first.txt"), b"first\n".to_vec()),
            Op::Remove(first.root().join("empty.bin")),
            Op::Write(
                at(second.root(), "src/main.rs"),
                b"fn main() { second(); }\n".to_vec(),
            ),
        ],
    );
    first.commit(Some("first")).expect("first commit");
    let merged = second.commit(Some("second")).expect("second commit");
    assert!(merged.merged, "{merged:?}");
    assert!(matches!(merged.outcome, Outcome::Committed { .. }));
    assert_eq!(merged.rebased.updated, 1, "{:?}", merged.rebased);
    // The Branch now holds both Workspaces' changes.
    let (saved_shape, saved_digests) = fixture.saved("work");
    assert!(saved_shape.contains_key("from-first.txt"));
    assert!(!saved_shape.contains_key("empty.bin"));
    assert_eq!(
        saved_digests["README.md"],
        *blake3::hash(b"# changed by first\n").as_bytes()
    );
    assert_eq!(
        saved_digests["src/main.rs"],
        *blake3::hash(b"fn main() { second(); }\n").as_bytes()
    );
    // And the second Workspace's directory shows the new baseline.
    assert_eq!(shape(second.root()), saved_shape);
    assert_eq!(tree_digests(second.root()), saved_digests);
    assert_eq!(second.status().expect("status").changes, Vec::new());
    first.discard().expect("discard");
    second.discard().expect("discard");
    fixture.close();
}

#[test]
fn unmount_and_discard_never_delete_history() {
    let fixture = Fixture::new("m1-history");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    edit(&fixture, &root);
    let committed = workspace.commit(Some("edits")).expect("commit");
    let Outcome::Committed { commit, .. } = committed.outcome else {
        panic!("expected a Commit");
    };
    let saved = fixture.saved("work");
    ops::run(
        fixture.home.path(),
        &[Op::Write(
            root.join("uncommitted.txt"),
            b"left behind\n".to_vec(),
        )],
    );
    workspace.unmount().expect("unmount");
    assert_eq!(fixture.head().as_deref(), Some(commit.as_str()));
    assert_eq!(fixture.saved("work"), saved);
    assert_eq!(fixture.saved(&commit), saved);
    assert!(!fixture.saved(&fixture.imported).0.contains_key("new.txt"));

    // The directory can be served again; the uncommitted file is still a change.
    let (resumed, _) = Workspace::resume(&fixture.engine, &root, None, 0).expect("resume");
    let changes = resumed.status().expect("status").changes;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].path, "uncommitted.txt");
    resumed.discard().expect("discard");
    assert!(!root.exists());
    assert_eq!(fixture.head().as_deref(), Some(commit.as_str()));
    assert_eq!(fixture.saved("work"), saved);
    fixture.close();
}

#[test]
fn a_commit_or_layer_identity_mounts_on_a_new_branch() {
    let fixture = Fixture::new("m1-at-commit");
    let workspace = fixture.mount("ws");
    edit(&fixture, workspace.root());
    let committed = workspace.commit(Some("edits")).expect("commit");
    let Outcome::Committed { commit, .. } = committed.outcome else {
        panic!("expected a Commit");
    };
    workspace.discard().expect("discard");
    // Move the Branch on, so its head is neither identity mounted below.
    let later = fixture.mount("later");
    ops::run(
        fixture.home.path(),
        &[Op::Write(
            later.root().join("later.txt"),
            b"later\n".to_vec(),
        )],
    );
    later.commit(Some("later")).expect("commit");
    later.discard().expect("discard");

    // The Commit's tree, on a new Branch forked at it.
    let at_commit = fixture.mount_from(
        "at-commit",
        From::Commit {
            commit: &commit,
            branch: "from-commit",
        },
    );
    let (saved_shape, saved_digests) = fixture.saved(&commit);
    assert_eq!(shape(at_commit.root()), saved_shape);
    assert_eq!(tree_digests(at_commit.root()), saved_digests);
    assert!(saved_shape.contains_key("new.txt") && !saved_shape.contains_key("later.txt"));
    assert_eq!(at_commit.status().expect("status").branch, "from-commit");
    at_commit.discard().expect("discard");

    // The Layer Init published, likewise.
    let at_layer = fixture.mount_from(
        "at-layer",
        From::Commit {
            commit: &fixture.imported,
            branch: "from-layer",
        },
    );
    let (saved_shape, saved_digests) = fixture.saved(&fixture.imported);
    assert_eq!(shape(at_layer.root()), saved_shape);
    assert_eq!(tree_digests(at_layer.root()), saved_digests);
    assert_eq!(saved_shape.len(), {
        let directories: std::collections::BTreeSet<&str> = fixture
            .files
            .iter()
            .flat_map(|(path, _)| path.match_indices('/').map(|(at, _)| &path[..at]))
            .collect();
        fixture.files.len() + directories.len()
    });
    at_layer.discard().expect("discard");
    fixture.close();
}
