//! Review repros for the ProjFS prototype (local scratch branch only; not
//! part of the reviewed PR). Each test states the behavior a reader of
//! REPORT.md would expect; a failing test is a reproduced finding.
#![cfg(windows)]

mod support;

use layerfs_projfs::workspace::Outcome;
use layerfs_projfs::{From, MountRequest, Workspace};
use std::os::windows::fs::FileExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use support::fixture::{shape, Fixture};
use support::ops::{self, Op};
use support::{at, tree_digests};

#[test]
fn child_process_entry() {
    ops::child_entry();
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let began = Instant::now();
    while !ready() {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// R1. A full file (already committed) is opened for writing AFTER the last
/// Commit, written, and still open at the next Commit. ProjFS reports a
/// change to a full file only when the handle closes.
#[test]
fn r1_a_write_through_a_still_open_handle_reaches_the_next_commit() {
    let fixture = Fixture::new("rv-open-handle");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[Op::Write(root.join("log.txt"), b"first line\n".to_vec())],
    );
    let first = ws.commit(None).expect("commit 1");
    assert!(matches!(first.outcome, Outcome::Committed { .. }));
    assert_eq!(ws.status().expect("status").changes, Vec::new());

    let (mut writer, _) = ops::spawn(
        fixture.home.path(),
        &[Op::Hold(root.join("log.txt"), b"SECOND".to_vec(), 6000)],
    );
    wait_for("the writer wrote", || {
        std::fs::read(root.join("log.txt")).is_ok_and(|bytes| bytes.starts_with(b"SECOND"))
    });
    let on_disk = std::fs::read(root.join("log.txt")).expect("read");
    let status = ws.status().expect("status").changes;
    let second = ws.commit(None);
    let still_open = writer.try_wait().expect("child").is_none();
    let (_, digests) = fixture.saved("work");
    let committed = digests["log.txt"];
    eprintln!(
        "R1 on disk: {:?}; status before commit 2: {status:?}; commit 2: {:?}; writer still open: {still_open}",
        String::from_utf8_lossy(&on_disk),
        second.as_ref().map(|c| c.outcome.clone())
    );
    writer.wait().expect("child");
    ws.discard().expect("discard");
    fixture.close();
    assert!(still_open, "the writer closed before the Commit; rerun");
    assert_eq!(
        committed,
        digest(&on_disk),
        "the second Commit does not hold the bytes that were on disk"
    );
}

/// R2. A directory this Workspace created and committed is changed inside
/// and then renamed before the next Commit.
#[test]
fn r2_changes_inside_a_directory_renamed_afterwards_reach_the_commit() {
    let fixture = Fixture::new("rv-dir-rename");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[
            Op::Mkdir(root.join("pkg")),
            Op::Write(at(&root, "pkg/a.txt"), b"one".to_vec()),
            Op::Write(at(&root, "pkg/gone.txt"), b"gone".to_vec()),
        ],
    );
    let first = ws.commit(None).expect("commit 1");
    assert!(matches!(first.outcome, Outcome::Committed { .. }));
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(at(&root, "pkg/a.txt"), b"two!".to_vec()),
            Op::Write(at(&root, "pkg/new.txt"), b"new".to_vec()),
            Op::Remove(at(&root, "pkg/gone.txt")),
            Op::Rename(root.join("pkg"), root.join("pkg2")),
        ],
    );
    let status = ws.status().expect("status").changes;
    let second = ws.commit(None);
    let (saved_shape, saved_digests) = fixture.saved("work");
    let (disk_shape, disk_digests) = (shape(&root), tree_digests(&root));
    let only = |map: &std::collections::BTreeMap<String, (bool, u64)>| -> Vec<String> {
        map.keys()
            .filter(|k| k.starts_with("pkg"))
            .cloned()
            .collect()
    };
    eprintln!(
        "R2 status: {status:?}\nR2 commit 2: {:?}\nR2 on disk: {:?}\nR2 in Store: {:?}",
        second.as_ref().map(|c| c.outcome.clone()),
        only(&disk_shape),
        only(&saved_shape)
    );
    let after = ws.status().expect("status").changes;
    eprintln!("R2 status after commit 2: {after:?}");
    ws.discard().expect("discard");
    fixture.close();
    let stale = saved_digests.get("pkg2/a.txt") == Some(&digest(b"one"));
    eprintln!(
        "R2 pkg2/a.txt in Store equals disk: {}; still the old `one`: {stale}",
        saved_digests.get("pkg2/a.txt") == disk_digests.get("pkg2/a.txt")
    );
    assert_eq!(
        saved_digests.get("pkg2/a.txt"),
        disk_digests.get("pkg2/a.txt"),
        "pkg2/a.txt content (disk holds `two!`)"
    );
    assert_eq!(only(&saved_shape), only(&disk_shape), "names");
}

const A1: &[u8] = b"fn main() { a1(); }\n";
const B2: &[u8] = b"fn main() { b2(); }\n";

/// R3a. Workspace A committed a file (it stays a full file on A's disk).
/// Workspace B then changes that file and commits. A commits something
/// else, which merges B's Commit. A's directory should show B's version.
#[test]
fn r3a_a_file_this_workspace_committed_follows_a_later_merge() {
    let fixture = Fixture::new("rv-stale-full");
    let a = fixture.mount("a");
    ops::run(
        fixture.home.path(),
        &[Op::Write(at(a.root(), "src/main.rs"), A1.to_vec())],
    );
    a.commit(Some("a1")).expect("a1");
    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[Op::Write(at(b.root(), "src/main.rs"), B2.to_vec())],
    );
    let b2 = b.commit(Some("b2")).expect("b2");
    assert!(!b2.merged);
    ops::run(
        fixture.home.path(),
        &[Op::Write(a.root().join("other.txt"), b"other\n".to_vec())],
    );
    let merged = a.commit(Some("a3")).expect("a3");
    assert!(merged.merged, "{merged:?}");
    let (_, digests) = fixture.saved("work");
    assert_eq!(digests["src/main.rs"], digest(B2), "the Branch holds b2");
    let disk = std::fs::read(at(a.root(), "src/main.rs")).expect("read");
    let status = a.status().expect("status").changes;
    eprintln!(
        "R3a A's disk after the merge: {:?}; status: {status:?}; rebased: {:?}",
        String::from_utf8_lossy(&disk),
        merged.rebased
    );
    // What the next Commit does once the user saves the stale file again
    // (an editor writing back what it shows): is B's change kept?
    ops::run(
        fixture.home.path(),
        &[Op::Write(at(a.root(), "src/main.rs"), disk.clone())],
    );
    let again = a.commit(Some("a4"));
    let (_, digests) = fixture.saved("work");
    eprintln!(
        "R3a after re-saving the file A showed: commit {:?}; Branch holds b2: {}",
        again.as_ref().map(|c| c.outcome.clone()),
        digests["src/main.rs"] == digest(B2)
    );
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
    assert_eq!(
        String::from_utf8_lossy(&disk),
        String::from_utf8_lossy(B2),
        "A's directory does not show the base A is on, and status is clean"
    );
}

/// R3b. A Workspace with NOTHING uncommitted is unmounted. Another
/// Workspace changes a file the first had committed, and brings back a file
/// the first had deleted (and committed). The first is resumed and commits.
#[test]
fn r3b_a_restart_with_nothing_uncommitted_changes_nothing_on_the_branch() {
    let fixture = Fixture::new("rv-restart-revert");
    let a = fixture.mount("a");
    let a_root = a.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(at(&a_root, "src/main.rs"), A1.to_vec()),
            Op::Remove(a_root.join("empty.bin")),
        ],
    );
    a.commit(Some("a1")).expect("a1");
    assert_eq!(a.status().expect("status").changes, Vec::new());
    a.unmount().expect("unmount");

    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(at(b.root(), "src/main.rs"), B2.to_vec()),
            Op::Write(b.root().join("empty.bin"), b"back".to_vec()),
        ],
    );
    b.commit(Some("b2")).expect("b2");
    b.discard().expect("discard");
    let (before_shape, before_digests) = fixture.saved("work");
    assert_eq!(before_digests["src/main.rs"], digest(B2));
    assert!(before_shape.contains_key("empty.bin"));

    let (a, found) = Workspace::resume(&fixture.engine, &a_root, None, 0).expect("resume");
    let status = a.status().expect("status").changes;
    let outcome = a.commit(Some("after restart"));
    let (after_shape, after_digests) = fixture.saved("work");
    eprintln!(
        "R3b scan found {found}; status after resume: {status:?}; commit: {:?}",
        outcome.as_ref().map(|c| c.outcome.clone())
    );
    eprintln!(
        "R3b Branch after: main.rs is b2: {}, is a1: {}; empty.bin present: {}",
        after_digests["src/main.rs"] == digest(B2),
        after_digests["src/main.rs"] == digest(A1),
        after_shape.contains_key("empty.bin")
    );
    a.discard().expect("discard");
    fixture.close();
    assert_eq!(
        after_digests["src/main.rs"],
        digest(B2),
        "the restart reverted B's committed change"
    );
    assert!(
        after_shape.contains_key("empty.bin"),
        "the restart deleted the file B had brought back"
    );
}

/// R4. The guard `capture::bytes` relies on for a file another process has
/// open for writing: size and last-write time read from a handle before and
/// after. Does an in-place overwrite of the same length change either?
#[test]
fn r4_size_and_time_show_an_in_place_overwrite_by_an_open_writer() {
    let fixture = Fixture::new("rv-stamp");
    let ws = fixture.mount("ws");
    let mut results = Vec::new();
    for (label, path) in [
        ("plain NTFS", fixture.home.path().join("plain.bin")),
        ("inside the Workspace", ws.root().join("inside.bin")),
    ] {
        std::fs::write(&path, vec![b'a'; 1 << 20]).expect("write");
        std::thread::sleep(Duration::from_millis(100));
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("writer");
        let reader = std::fs::File::open(&path).expect("reader");
        let stamp = |file: &std::fs::File| {
            let metadata = file.metadata().expect("metadata");
            (metadata.len(), metadata.modified().expect("modified"))
        };
        let before = stamp(&reader);
        writer.seek_write(&[b'b'; 4096], 0).expect("overwrite");
        let after = stamp(&reader);
        std::thread::sleep(Duration::from_millis(300));
        let later = stamp(&reader);
        let mut seen = [0u8; 1];
        reader.seek_read(&mut seen, 0).expect("read");
        eprintln!(
            "R4 {label}: reader sees new byte: {}; stamp changed at once: {}; after 300 ms: {}",
            seen[0] == b'b',
            before != after,
            before != later
        );
        results.push((label, before != after));
        drop(writer);
        let closed = stamp(&reader);
        eprintln!(
            "R4 {label}: stamp changed after the writer closed: {}",
            before != closed
        );
    }
    ws.discard().expect("discard");
    fixture.close();
    for (label, changed) in results {
        assert!(
            changed,
            "{label}: an in-place overwrite left size and time unchanged"
        );
    }
}

/// R5. A directory junction created inside the Workspace (as pnpm and
/// `mklink /J` do), pointing outside it.
#[test]
fn r5_a_junction_in_the_workspace_does_not_block_commit() {
    let fixture = Fixture::new("rv-junction");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    let target = fixture.home.path().join("tree").join("src");
    let made = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(root.join("linked"))
        .arg(&target)
        .output()
        .expect("mklink");
    eprintln!(
        "R5 mklink: {} {} {}",
        made.status,
        String::from_utf8_lossy(&made.stdout).trim(),
        String::from_utf8_lossy(&made.stderr).trim()
    );
    ops::run(
        fixture.home.path(),
        &[Op::Write(root.join("ordinary.txt"), b"ordinary".to_vec())],
    );
    let status = ws.status().map(|s| s.changes);
    let outcome = ws.commit(None);
    let text = match &outcome {
        Ok(committed) => format!("{:?}", committed.outcome),
        Err(refusal) => refusal.to_string(),
    };
    eprintln!("R5 status: {status:?}\nR5 commit: {text}");
    let (saved, _) = fixture.saved("work");
    let linked: Vec<&String> = saved.keys().filter(|k| k.starts_with("linked")).collect();
    eprintln!("R5 in Store under `linked`: {linked:?}");
    let target_before = target.join("main.rs").exists();
    let discarded = ws.discard();
    eprintln!(
        "R5 discard: {discarded:?}; junction target intact: {}",
        target_before && target.join("main.rs").exists()
    );
    fixture.close();
    assert!(made.status.success(), "the junction could not be created");
    assert!(outcome.is_ok(), "{text}");
}

/// R6. What status and Commit cost once the tree has been listed (as a
/// search or `git status` does): the attribute sweep lists every directory
/// that is on disk. Prints timings; asserts nothing about them.
#[test]
fn r6_status_and_commit_after_the_tree_was_listed() {
    const DIRECTORIES: usize = 2000;
    let home = support::Scratch::new("rv-sweep");
    let tree = home.path().join("tree");
    for index in 0..DIRECTORIES {
        let directory = tree
            .join(format!("g{:02}", index % 40))
            .join(format!("d{index:04}"));
        std::fs::create_dir_all(&directory).expect("directory");
        std::fs::write(directory.join("file.txt"), format!("file {index}\n")).expect("file");
    }
    let store = home.path().join("store.sqlite");
    layerfs_projfs::store::init_project(&store, "demo", "work", &tree).expect("Project Init");
    let engine = std::sync::Arc::new(
        layerfs_projfs::store::Engine::open(&store, &home.path().join("overlay")).expect("engine"),
    );
    let root = home.path().join("ws");
    let ws = Workspace::mount(
        &engine,
        MountRequest {
            project: "demo",
            from: From::Branch("work"),
            root: &root,
            state: None,
            threads: 0,
        },
    )
    .expect("mount");
    let timed = |what: &str, run: &mut dyn FnMut()| {
        let began = Instant::now();
        run();
        let enumerations = ws.projection().provider().served().enumerations;
        eprintln!(
            "R6 {what}: {:.1} ms (provider enumerations so far: {enumerations})",
            began.elapsed().as_secs_f64() * 1000.0
        );
    };
    let edit = |name: &str| {
        ops::run(
            home.path(),
            &[Op::Write(
                at(&root, "g00/d0000/file.txt"),
                name.as_bytes().to_vec(),
            )],
        );
    };
    edit("first edit\n");
    timed("status, fresh mount, 1 file changed", &mut || {
        ws.status().expect("status");
    });
    timed("commit, fresh mount, 1 file changed", &mut || {
        ws.commit(None).expect("commit");
    });
    let listed = Command::new("cmd")
        .args(["/c", "dir", "/s", "/b", "/a"])
        .arg(&root)
        .stdout(Stdio::null())
        .status()
        .expect("dir");
    assert!(listed.success());
    eprintln!("R6 the tree was listed once by another process ({DIRECTORIES} leaf directories)");
    edit("second edit\n");
    timed("status, after listing, 1 file changed", &mut || {
        ws.status().expect("status");
    });
    timed("commit, after listing, 1 file changed", &mut || {
        ws.commit(None).expect("commit");
    });
    ws.discard().expect("discard");
    std::sync::Arc::try_unwrap(engine)
        .ok()
        .expect("engine")
        .close()
        .expect("close");
}

/// R7. The baseline update after a merge, while another process holds a
/// read handle on a hydrated placeholder the merge changed.
#[test]
fn r7_a_placeholder_open_for_reading_follows_a_merge() {
    let fixture = Fixture::new("rv-open-placeholder");
    let (first, second) = (fixture.mount("a"), fixture.mount("b"));
    let readme = second.root().join("README.md");
    let _ = std::fs::read(&readme).expect("hydrate");
    let marker = fixture.home.path().join("reader-ready");
    let mut reader = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "$s=[System.IO.File]::Open('{}','Open','Read','ReadWrite,Delete'); \
             Set-Content '{}' 'ready'; Start-Sleep -Milliseconds 5000; $s.Dispose()",
            readme.display(),
            marker.display()
        ))
        .stdout(Stdio::null())
        .spawn()
        .expect("powershell");
    wait_for("the reader holds the file", || marker.exists());
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(
                first.root().join("README.md"),
                b"# changed by first\n".to_vec(),
            ),
            Op::Write(at(second.root(), "src/main.rs"), B2.to_vec()),
        ],
    );
    first.commit(Some("first")).expect("first");
    let merged = second.commit(Some("second")).expect("second");
    assert!(merged.merged);
    let held = reader.try_wait().expect("child").is_none();
    eprintln!(
        "R7 reader still holds the file: {held}; rebased: {:?}",
        merged.rebased
    );
    reader.wait().expect("reader");
    let disk = std::fs::read(&readme).expect("read");
    let status = second.status().expect("status").changes;
    eprintln!(
        "R7 after the reader closed: disk {:?}; status {status:?}",
        String::from_utf8_lossy(&disk)
    );
    // A later Commit of the Workspace: does anything bring the file up to date?
    let again = second.commit(None).expect("again");
    let later = std::fs::read(&readme).expect("read");
    eprintln!(
        "R7 after another Commit ({:?}): disk {:?}",
        again.outcome,
        String::from_utf8_lossy(&later)
    );
    first.discard().expect("discard");
    second.discard().expect("discard");
    fixture.close();
    assert!(held, "the reader closed before the merge; rerun");
    assert_eq!(
        String::from_utf8_lossy(&later),
        "# changed by first\n",
        "the directory keeps the old content of a file the merge changed"
    );
}

/// R8. `resume` given one Workspace's state directory and another
/// Workspace's directory. The state holds the instance identity `mount`
/// wrote; is it compared with the directory?
#[test]
fn r8_resume_refuses_a_state_directory_of_another_workspace() {
    let fixture = Fixture::new("rv-wrong-root");
    let a = fixture.mount("a");
    let b = fixture.mount_from(
        "b",
        From::Commit {
            commit: &fixture.imported,
            branch: "other",
        },
    );
    let (a_root, a_state) = (a.root().to_owned(), a.state().to_owned());
    let (b_root, b_state) = (b.root().to_owned(), b.state().to_owned());
    ops::run(
        fixture.home.path(),
        &[Op::Write(b_root.join("b-only.txt"), b"b".to_vec())],
    );
    a.unmount().expect("unmount");
    b.unmount().expect("unmount");
    let crossed = Workspace::resume(&fixture.engine, &b_root, Some(&a_state), 0);
    let accepted = crossed.is_ok();
    if let Ok((crossed, _)) = crossed {
        let status = crossed.status().expect("status");
        eprintln!(
            "R8 resume of b's directory with a's state was accepted: Branch {}, changes {:?}",
            status.branch, status.changes
        );
        let discarded = crossed.discard();
        eprintln!(
            "R8 discard: {discarded:?}; b's directory exists: {}; a's state exists: {}; a's directory exists: {}",
            b_root.exists(),
            a_state.exists(),
            a_root.exists()
        );
    }
    // Clean up whatever is left, through the proper pairs where they exist.
    for (root, state) in [(&a_root, &a_state), (&b_root, &b_state)] {
        if let Ok((workspace, _)) = Workspace::resume(&fixture.engine, root, Some(state), 0) {
            let _ = workspace.discard();
        }
    }
    eprintln!(
        "R8 left behind: a's directory {}, b's state {}",
        a_root.exists(),
        b_state.exists()
    );
    fixture.close();
    assert!(
        !accepted,
        "resume served one Workspace's directory with another's state"
    );
}
