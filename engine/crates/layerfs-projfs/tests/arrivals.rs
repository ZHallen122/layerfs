//! A name taken over by another item between two Commits, and what a merge
//! brings to a directory this Workspace wrote into. In every case the
//! directory and the Branch must hold the same after the Commit, and a
//! status must not call the directory clean while they differ.
#![cfg(windows)]

mod support;

use layerfs_projfs::workspace::{CommitRefusal, Outcome};
use layerfs_projfs::Workspace;
use support::fixture::{shape, Fixture};
use support::ops::{self, Op};
use support::{at, tree_digests};

#[test]
fn child_process_entry() {
    ops::child_entry();
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// The directory and the Branch hold the same names and bytes.
fn agree(fixture: &Fixture, ws: &Workspace, what: &str) {
    let (saved_shape, saved_digests) = fixture.saved("work");
    assert_eq!(shape(ws.root()), saved_shape, "{what}: names and sizes");
    assert_eq!(tree_digests(ws.root()), saved_digests, "{what}: bytes");
    let status = ws.status().expect("status").changes;
    assert!(
        status.is_empty(),
        "{what}: status after the Commit: {status:?}"
    );
}

/// A projected file is renamed to the name of another projected file that
/// was deleted first. The file under the name is then a placeholder, whose
/// bytes are another path's: once never read, once read before.
#[test]
fn a_projected_file_renamed_over_a_deleted_name_reaches_the_commit() {
    let fixture = Fixture::new("arr-placeholder");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    let launch = "docs/\u{1f680}-launch.txt";
    ops::run(
        fixture.home.path(),
        &[
            Op::Remove(root.join("README.md")),
            Op::Rename(at(&root, "src/main.rs"), root.join("README.md")),
            Op::Read(at(&root, launch)),
            Op::Remove(root.join("empty.bin")),
            Op::Rename(at(&root, launch), root.join("empty.bin")),
        ],
    );
    let status = ws.status().expect("status").changes;
    let named = |path: &str| status.iter().any(|change| change.path == path);
    assert!(
        named("README.md") && named("empty.bin"),
        "a status names both replaced files: {status:?}"
    );
    let done = ws.commit(Some("renamed over")).expect("commit");
    assert!(matches!(done.outcome, Outcome::Committed { .. }));
    let (saved_shape, saved_digests) = fixture.saved("work");
    assert_eq!(saved_digests["README.md"], digest(b"fn main() {}\n"));
    assert_eq!(saved_digests["empty.bin"], digest(b"emoji name\n"));
    assert!(!saved_shape.contains_key("src/main.rs"));
    assert!(!saved_shape.contains_key(launch));
    agree(&fixture, &ws, "renamed over");
    ws.discard().expect("discard");
    fixture.close();
}

fn renamed_over_without_reports(label: &str, lose_the_journal: bool) {
    let fixture = Fixture::new(label);
    let ws = fixture.mount("ws");
    let (root, state) = (ws.root().to_owned(), ws.state().to_owned());
    ops::run(
        fixture.home.path(),
        &[
            Op::Remove(root.join("README.md")),
            Op::Rename(at(&root, "src/main.rs"), root.join("README.md")),
        ],
    );
    ws.unmount().expect("unmount");
    std::fs::write(state.join("journal.log"), b"").expect("clear the reports");
    if lose_the_journal {
        let anchor = state.join("anchor");
        let text = std::fs::read_to_string(&anchor).expect("anchor");
        let forged: String = text
            .lines()
            .map(|line| match line.starts_with("journal=") {
                true => "journal=1\n".to_owned(),
                false => format!("{line}\n"),
            })
            .collect();
        assert_ne!(forged, text);
        std::fs::write(&anchor, forged).expect("anchor");
    }
    let (ws, _) = Workspace::resume(&fixture.engine, &root, None, 0).expect("resume");
    let status = ws.status().expect("status").changes;
    assert!(
        status.iter().any(|change| change.path == "README.md"),
        "a status names the replaced file: {status:?}"
    );
    ws.commit(Some("renamed over")).expect("commit");
    assert_eq!(
        fixture.saved("work").1["README.md"],
        digest(b"fn main() {}\n")
    );
    agree(&fixture, &ws, label);
    ws.discard().expect("discard");
    fixture.close();
}

/// The same with the reports of ProjFS lost: the change journal alone has
/// to find that the name holds another file now.
#[test]
fn a_projected_file_renamed_over_a_deleted_name_is_found_without_the_reports() {
    renamed_over_without_reports("arr-placeholder-nolog", false);
}

/// And with the change journal lost as well: everything on disk is
/// compared, and a placeholder that still has the size and time of the file
/// it was made for is told from the base's file at its name. (Two files of
/// one size and one time are not told apart this way; see the report.)
#[test]
fn a_projected_file_renamed_over_a_deleted_name_is_found_without_the_journal() {
    renamed_over_without_reports("arr-placeholder-nojournal", true);
}

/// A second name is given to a committed file, in place of another
/// committed file that was deleted first.
#[test]
fn a_hard_link_made_over_a_deleted_name_reaches_the_commit() {
    let fixture = Fixture::new("arr-link");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(root.join("a.txt"), b"first".to_vec()),
            Op::Write(root.join("b.txt"), b"second".to_vec()),
        ],
    );
    ws.commit(Some("two files")).expect("commit 1");
    ops::run(
        fixture.home.path(),
        &[
            Op::Remove(root.join("b.txt")),
            Op::HardLink(root.join("a.txt"), root.join("b.txt")),
        ],
    );
    let status = ws.status().expect("status").changes;
    assert!(
        status.iter().any(|change| change.path == "b.txt"),
        "a status names the replaced file: {status:?}"
    );
    ws.commit(Some("linked over")).expect("commit 2");
    assert_eq!(fixture.saved("work").1["b.txt"], digest(b"first"));
    agree(&fixture, &ws, "linked over");
    // A later write through either name is one change to both.
    ops::run(
        fixture.home.path(),
        &[Op::Write(root.join("b.txt"), b"third".to_vec())],
    );
    ws.commit(Some("written through the link"))
        .expect("commit 3");
    let saved = fixture.saved("work").1;
    assert_eq!(saved["a.txt"], digest(b"third"));
    assert_eq!(saved["b.txt"], digest(b"third"));
    agree(&fixture, &ws, "written through the link");
    ws.discard().expect("discard");
    fixture.close();
}

/// A committed directory is renamed onto the name of a committed directory
/// that was removed, both made in this Workspace: no projected item is
/// involved, and one file has the same name, size and age in both.
#[test]
fn a_local_directory_renamed_over_a_local_directory_reaches_the_commit() {
    let fixture = Fixture::new("arr-local-dirs");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[
            Op::Mkdir(at(&root, "out/sub")),
            Op::Write(at(&root, "out/same.txt"), b"aaaa".to_vec()),
            Op::Write(at(&root, "out/sub/old.txt"), b"old".to_vec()),
            Op::Mkdir(at(&root, "out.new/sub")),
            Op::Write(at(&root, "out.new/same.txt"), b"bbbb".to_vec()),
            Op::Write(at(&root, "out.new/sub/new.txt"), b"new".to_vec()),
        ],
    );
    ws.commit(Some("both")).expect("commit 1");
    ops::run(
        fixture.home.path(),
        &[
            Op::RemoveTree(root.join("out")),
            Op::Rename(root.join("out.new"), root.join("out")),
        ],
    );
    let status = ws.status().expect("status").changes;
    assert!(
        status.iter().any(|change| change.path == "out/same.txt"),
        "a status names the file whose bytes changed: {status:?}"
    );
    ws.commit(Some("swapped")).expect("commit 2");
    let (saved_shape, saved_digests) = fixture.saved("work");
    assert_eq!(saved_digests["out/same.txt"], digest(b"bbbb"));
    assert!(saved_shape.contains_key("out/sub/new.txt"));
    assert!(!saved_shape.contains_key("out/sub/old.txt"));
    assert!(!saved_shape.contains_key("out.new"));
    agree(&fixture, &ws, "swapped");
    ws.discard().expect("discard");
    fixture.close();
}

/// Another Workspace adds a file below a directory this Workspace made and
/// committed. ProjFS projects nothing into such a directory, so the merge
/// has to bring the file to the disk; a later Commit from here must not
/// remove it from the Branch.
#[test]
fn a_file_added_elsewhere_below_a_directory_made_here_arrives_with_the_merge() {
    let fixture = Fixture::new("arr-merge-add");
    let a = fixture.mount("a");
    ops::run(
        fixture.home.path(),
        &[
            Op::Mkdir(a.root().join("pkg")),
            Op::Write(at(a.root(), "pkg/a.txt"), b"a1".to_vec()),
        ],
    );
    a.commit(Some("a1")).expect("a1");
    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(at(b.root(), "pkg/b.txt"), b"b1".to_vec()),
            Op::Mkdir(at(b.root(), "pkg/more")),
            Op::Write(at(b.root(), "pkg/more/c.txt"), b"c1".to_vec()),
            // And where ProjFS does project: the root, a base directory.
            Op::Write(b.root().join("top.txt"), b"top".to_vec()),
            Op::Write(at(b.root(), "docs/new.txt"), b"doc".to_vec()),
        ],
    );
    b.commit(Some("b1")).expect("b1");
    ops::run(
        fixture.home.path(),
        &[Op::Write(a.root().join("other.txt"), b"other".to_vec())],
    );
    let merged = a.commit(Some("a2")).expect("a2");
    assert!(merged.merged, "{merged:?}");
    eprintln!("merge-add rebased: {:?}", merged.rebased);
    agree(&fixture, &a, "after the merge");
    assert_eq!(
        std::fs::read(at(a.root(), "pkg/more/c.txt")).expect("read"),
        b"c1"
    );
    ops::run(
        fixture.home.path(),
        &[Op::Write(at(a.root(), "pkg/a.txt"), b"a3".to_vec())],
    );
    a.commit(Some("a3")).expect("a3");
    let saved = fixture.saved("work").1;
    assert_eq!(
        saved["pkg/b.txt"],
        digest(b"b1"),
        "the Branch kept B's file"
    );
    assert_eq!(saved["pkg/a.txt"], digest(b"a3"));
    agree(&fixture, &a, "after a later Commit");
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
}

/// Another Workspace deletes a file this Workspace committed, and a
/// projected directory this Workspace looked into and read from.
#[test]
fn what_another_workspace_deleted_is_gone_after_the_merge() {
    let fixture = Fixture::new("arr-merge-delete");
    let a = fixture.mount("a");
    let launch = "docs/\u{1f680}-launch.txt";
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(a.root().join("mine.txt"), b"mine".to_vec()),
            Op::Read(at(a.root(), launch)),
        ],
    );
    a.commit(Some("mine")).expect("a1");
    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[
            Op::Remove(b.root().join("mine.txt")),
            Op::RemoveTree(b.root().join("docs")),
        ],
    );
    b.commit(Some("deleted")).expect("b1");
    ops::run(
        fixture.home.path(),
        &[Op::Write(a.root().join("other.txt"), b"other".to_vec())],
    );
    let merged = a.commit(Some("a2")).expect("a2");
    assert!(merged.merged, "{merged:?}");
    eprintln!("merge-delete rebased: {:?}", merged.rebased);
    assert!(!a.root().join("mine.txt").exists());
    assert!(!a.root().join("docs").exists());
    agree(&fixture, &a, "after the merge");
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
}

/// A Conflict publishes nothing, and the directory then shows the Branch's
/// head with this Workspace's changes on top: also at a file this Workspace
/// rewrote without changing it, which the other Workspace did change.
#[test]
fn a_conflict_brings_the_other_workspaces_changes_to_the_directory() {
    let fixture = Fixture::new("arr-conflict");
    let (a, b) = (fixture.mount("a"), fixture.mount("b"));
    ops::run(
        fixture.home.path(),
        &[
            // A: one real edit, and one file rewritten with the same bytes.
            Op::Write(
                at(a.root(), "src/main.rs"),
                b"fn main() { a(); }\n".to_vec(),
            ),
            Op::Write(a.root().join("README.md"), b"# fixture\n".to_vec()),
            // B: an edit of the same line, and of the file A only rewrote.
            Op::Write(
                at(b.root(), "src/main.rs"),
                b"fn main() { b(); }\n".to_vec(),
            ),
            Op::Write(b.root().join("README.md"), b"# theirs\n".to_vec()),
        ],
    );
    b.commit(Some("theirs")).expect("b");
    let refused = a.commit(Some("mine")).expect_err("a conflict");
    let CommitRefusal::Conflict { conflicts, .. } = &refused else {
        panic!("expected a Conflict: {refused}");
    };
    assert_eq!(conflicts, &["/src/main.rs".to_owned()]);
    let marked = std::fs::read_to_string(at(a.root(), "src/main.rs")).expect("read");
    assert!(
        marked.contains("a();") && marked.contains("b();") && marked.contains("<<<<<<<"),
        "the conflicted file holds both sides: {marked:?}"
    );
    assert_eq!(
        std::fs::read(a.root().join("README.md")).expect("read"),
        b"# theirs\n",
        "the file A only rewrote shows the Branch's bytes"
    );
    // A resolves and commits: the Branch keeps B's README.
    ops::run(
        fixture.home.path(),
        &[Op::Write(
            at(a.root(), "src/main.rs"),
            b"fn main() { a(); b(); }\n".to_vec(),
        )],
    );
    a.commit(Some("resolved")).expect("resolved");
    let saved = fixture.saved("work").1;
    assert_eq!(saved["README.md"], digest(b"# theirs\n"));
    assert_eq!(saved["src/main.rs"], digest(b"fn main() { a(); b(); }\n"));
    agree(&fixture, &a, "after the resolution");
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
}

/// The provider stops after a merge was published and before the directory
/// followed it. The state directory then says which base the directory
/// still shows, and the next provider finishes the step.
#[test]
fn a_merge_the_directory_did_not_follow_is_finished_at_resume() {
    let fixture = Fixture::new("arr-unfinished");
    let a = fixture.mount("a");
    let (a_root, a_state) = (a.root().to_owned(), a.state().to_owned());
    ops::run(
        fixture.home.path(),
        &[
            Op::Read(a_root.join("README.md")),
            Op::Write(a_root.join("mine.txt"), b"mine".to_vec()),
            Op::Write(a_root.join("edited.txt"), b"committed".to_vec()),
        ],
    );
    a.commit(Some("mine")).expect("a1");
    let shown = a.status().expect("status").root;
    // Changed after the Commit, and not committed.
    ops::run(
        fixture.home.path(),
        &[Op::Write(a_root.join("edited.txt"), b"local".to_vec())],
    );
    a.unmount().expect("unmount");
    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(b.root().join("README.md"), b"# theirs\n".to_vec()),
            Op::Write(b.root().join("mine.txt"), b"theirs".to_vec()),
            Op::Write(b.root().join("edited.txt"), b"theirs too".to_vec()),
        ],
    );
    b.commit(Some("theirs")).expect("b1");
    let now = b.status().expect("status");
    // What the state directory holds when a provider stops between the
    // publication and the update of the directory.
    let record = a_state.join("workspace");
    let text = std::fs::read_to_string(&record).expect("record");
    assert!(text.contains(&format!("root={shown}\n")), "{text}");
    std::fs::write(
        &record,
        format!(
            "project=demo\nbranch=work\nroot={}\nhead={}\nshown={shown}\ntarget={}\n",
            now.root,
            now.head.clone().expect("head"),
            now.root
        ),
    )
    .expect("forge");
    let (a, found) = Workspace::resume(&fixture.engine, &a_root, None, 0).expect("resume");
    let status = a.status().expect("status");
    assert!(status.diverged.is_none(), "{:?}", status.diverged);
    assert_eq!(
        std::fs::read(a_root.join("README.md")).expect("read"),
        b"# theirs\n",
        "a file read before shows the new base"
    );
    assert_eq!(
        std::fs::read(a_root.join("mine.txt")).expect("read"),
        b"theirs",
        "a committed file shows the new base"
    );
    assert_eq!(
        std::fs::read(a_root.join("edited.txt")).expect("read"),
        b"local",
        "a file changed since its Commit is left alone"
    );
    let paths: Vec<&str> = status.changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["edited.txt"], "and is the one uncommitted change");
    assert_eq!(found, 1);
    assert!(
        !std::fs::read_to_string(&record)
            .expect("record")
            .contains("shown="),
        "the step is recorded as finished"
    );
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
}

/// A directory is removed and made again under its name, empty or with new
/// content: once a directory of the base, once one made and committed here.
/// What the old one held must not stay in the Branch.
#[test]
fn a_directory_removed_and_made_again_holds_only_what_is_in_it_now() {
    let fixture = Fixture::new("arr-remade");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[
            Op::Mkdir(at(&root, "out/sub")),
            Op::Write(at(&root, "out/old.txt"), b"old".to_vec()),
            Op::Write(at(&root, "out/sub/old.txt"), b"old".to_vec()),
        ],
    );
    ws.commit(Some("out")).expect("commit 1");
    ops::run(
        fixture.home.path(),
        &[
            Op::RemoveTree(root.join("out")),
            Op::Mkdir(root.join("out")),
            Op::RemoveTree(root.join("docs")),
            Op::Mkdir(root.join("docs")),
            Op::RemoveTree(root.join("src")),
            Op::Mkdir(root.join("src")),
            Op::Write(
                at(&root, "src/main.rs"),
                b"fn main() { again(); }\n".to_vec(),
            ),
        ],
    );
    let status = ws.status().expect("status").changes;
    let deleted = |path: &str| {
        status
            .iter()
            .any(|change| change.path == path && change.kind == layerfs_projfs::Kind::Deleted)
    };
    assert!(
        deleted("out/old.txt") && deleted("docs/\u{1f680}-launch.txt") && deleted("src/lib"),
        "a status names what the old directories held: {status:?}"
    );
    ws.commit(Some("made again")).expect("commit 2");
    let (saved_shape, saved_digests) = fixture.saved("work");
    let below = |prefix: &str| -> Vec<String> {
        saved_shape
            .keys()
            .filter(|path| path.starts_with(prefix))
            .cloned()
            .collect()
    };
    assert_eq!(below("out"), ["out"]);
    assert_eq!(below("docs"), ["docs"]);
    assert_eq!(below("src"), ["src", "src/main.rs"]);
    assert_eq!(
        saved_digests["src/main.rs"],
        digest(b"fn main() { again(); }\n")
    );
    agree(&fixture, &ws, "made again");
    ws.discard().expect("discard");
    fixture.close();
}

/// A conflicted merge that changes more paths than the engine lists for it
/// (4,096, in path order). The directory follows the two trees and not the
/// list, so the conflicted file, whose path sorts after the list's end,
/// still gets its marker block, and the Conflict is not lost at the next
/// Commit.
#[test]
fn a_conflict_among_more_changed_paths_than_the_engine_lists_reaches_the_directory() {
    const FILES: usize = 4200;
    let fixture = Fixture::new("arr-many");
    let (a, b) = (fixture.mount("a"), fixture.mount("b"));
    let mut theirs = vec![Op::Mkdir(b.root().join("bulk"))];
    theirs.extend((0..FILES).map(|n| {
        Op::Write(
            at(b.root(), &format!("bulk/f{n:04}.txt")),
            format!("{n}\n").into_bytes(),
        )
    }));
    theirs.push(Op::Write(
        at(b.root(), "src/main.rs"),
        b"fn main() { b(); }\n".to_vec(),
    ));
    ops::run(fixture.home.path(), &theirs);
    b.commit(Some("theirs")).expect("b");
    ops::run(
        fixture.home.path(),
        &[Op::Write(
            at(a.root(), "src/main.rs"),
            b"fn main() { a(); }\n".to_vec(),
        )],
    );
    let refused = a.commit(Some("mine")).expect_err("a conflict");
    let CommitRefusal::Conflict {
        conflicts, written, ..
    } = &refused
    else {
        panic!("expected a Conflict: {refused}");
    };
    assert_eq!(conflicts, &["/src/main.rs".to_owned()]);
    // The case is the one meant: the engine's list ends before the file.
    assert!(
        !written.iter().any(|path| path == "/src/main.rs"),
        "the engine listed {} paths, the conflicted one among them",
        written.len()
    );
    let marked = std::fs::read_to_string(at(a.root(), "src/main.rs")).expect("read");
    assert!(
        marked.contains("a();") && marked.contains("b();") && marked.contains("<<<<<<<"),
        "the conflicted file holds both sides: {marked:?}"
    );
    assert_eq!(
        std::fs::read(at(a.root(), &format!("bulk/f{:04}.txt", FILES - 1))).expect("read"),
        format!("{}\n", FILES - 1).into_bytes()
    );
    // A Commit without a resolution is still refused: the markers are there.
    let again = a.commit(Some("unresolved"));
    assert!(again.is_err(), "{again:?}");
    ops::run(
        fixture.home.path(),
        &[Op::Write(
            at(a.root(), "src/main.rs"),
            b"fn main() { a(); b(); }\n".to_vec(),
        )],
    );
    a.commit(Some("resolved")).expect("resolved");
    let (saved_shape, saved_digests) = fixture.saved("work");
    assert_eq!(
        saved_digests["src/main.rs"],
        digest(b"fn main() { a(); b(); }\n")
    );
    assert_eq!(
        saved_shape
            .keys()
            .filter(|path| path.starts_with("bulk/"))
            .count(),
        FILES
    );
    // Not `agree`: reading 4,200 files back through the mount is another
    // test's subject and most of this one's time.
    let status = a.status().expect("status").changes;
    assert!(status.is_empty(), "{status:?}");
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
}
