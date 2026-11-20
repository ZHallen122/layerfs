//! Second-round review repros (local scratch branch only; not part of the
//! reviewed PR): attacks on the change-journal capture, the record of
//! committed files and the restart refusal. A failing test is a reproduced
//! finding; a passing one disproves a suspicion.
#![cfg(windows)]

mod support;

use layerfs_projfs::workspace::Outcome;
use layerfs_projfs::Workspace;
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

fn powershell(script: &str) -> std::process::Child {
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdout(Stdio::null())
        .spawn()
        .expect("powershell")
}

fn outcome(result: &Result<layerfs_projfs::workspace::Committed, layerfs_projfs::workspace::CommitRefusal>) -> String {
    match result {
        Ok(done) => format!("{:?} warnings {:?}", done.outcome, done.warnings),
        Err(refusal) => refusal.to_string(),
    }
}

/// S1. A process keeps a file open for writing across a provider restart.
/// Its first write was committed before the restart; the tracker's memory
/// of "still open" is not in the state directory. A later write through the
/// same handle adds no journal record.
#[test]
fn s1_a_write_through_a_handle_held_across_a_restart_reaches_the_commit() {
    let fixture = Fixture::new("rv2-open-restart");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    let file = root.join("log.txt");
    ops::run(
        fixture.home.path(),
        &[Op::Write(file.clone(), b"0123456789\n".to_vec())],
    );
    ws.commit(None).expect("commit 1");
    let go = fixture.home.path().join("second-write");
    let mut writer = powershell(&format!(
        "$s=[System.IO.File]::Open('{}','Open','Write','ReadWrite'); \
         $s.WriteByte(65); $s.Flush(); \
         while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 20 }}; \
         $s.WriteByte(66); $s.Flush(); Start-Sleep -Milliseconds 8000; $s.Dispose()",
        file.display(),
        go.display()
    ));
    let starts = |prefix: &[u8]| std::fs::read(&file).is_ok_and(|bytes| bytes.starts_with(prefix));
    wait_for("the first write", || starts(b"A"));
    let second = ws.commit(None).expect("commit 2");
    assert!(matches!(second.outcome, Outcome::Committed { .. }));
    assert_eq!(second.captured.live, ["log.txt"]);

    // The provider stops and starts again; the writer keeps its handle.
    ws.unmount().expect("unmount");
    let (ws, found) = Workspace::resume(&fixture.engine, &root, None, 0).expect("resume");
    std::fs::write(&go, b"go").expect("signal");
    wait_for("the second write", || starts(b"AB"));
    let status = ws.status().expect("status");
    let third = ws.commit(None);
    let still_open = writer.try_wait().expect("child").is_none();
    let committed = fixture.saved("work").1["log.txt"];
    eprintln!(
        "S1 resume found {found}; diverged {:?}; status after the second write: {:?}; commit 3: {}",
        status.diverged,
        status.changes,
        outcome(&third)
    );
    writer.wait().expect("child");
    ws.discard().expect("discard");
    fixture.close();
    assert!(still_open, "the writer closed before the Commit; rerun");
    assert_eq!(
        committed,
        digest(b"AB23456789\n"),
        "the Commit after the restart does not hold the bytes on disk"
    );
}

fn swap_setup(label: &str) -> (Fixture, Workspace) {
    let fixture = Fixture::new(label);
    let ws = fixture.mount("ws");
    (fixture, ws)
}

fn compare(fixture: &Fixture, ws: &Workspace, what: &str, prefix: &str) -> (Vec<String>, Vec<String>, bool) {
    let root = ws.root().to_owned();
    let (saved_shape, saved_digests) = fixture.saved("work");
    let (disk_shape, disk_digests) = (shape(&root), tree_digests(&root));
    let only = |map: &std::collections::BTreeMap<String, (bool, u64)>| -> Vec<String> {
        map.keys().filter(|k| k.starts_with(prefix)).cloned().collect()
    };
    let same_bytes = disk_digests
        .iter()
        .filter(|(path, _)| path.starts_with(prefix))
        .all(|(path, bytes)| saved_digests.get(path) == Some(bytes));
    eprintln!(
        "{what} on disk: {:?}\n{what} in Store: {:?}\n{what} bytes equal: {same_bytes}",
        only(&disk_shape),
        only(&saved_shape)
    );
    (only(&disk_shape), only(&saved_shape), same_bytes)
}

/// S2. A directory the Branch holds is deleted and another directory, also
/// already committed, is renamed to its name: the "build into dist.new,
/// then swap it in" pattern, with a Commit between the two steps.
#[test]
fn s2_a_directory_swapped_in_under_an_existing_name_reaches_the_commit() {
    let (fixture, ws) = swap_setup("rv2-dir-swap");
    let root = ws.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[
            Op::Mkdir(root.join("docs.new")),
            Op::Write(at(&root, "docs.new/guide.txt"), b"new guide".to_vec()),
            Op::Write(at(&root, "docs.new/index.txt"), b"new index".to_vec()),
        ],
    );
    ws.commit(Some("staged")).expect("commit 1");
    ops::run(
        fixture.home.path(),
        &[
            Op::RemoveTree(root.join("docs")),
            Op::Rename(root.join("docs.new"), root.join("docs")),
        ],
    );
    let status = ws.status().expect("status").changes;
    let second = ws.commit(Some("swapped"));
    eprintln!("S2 status: {status:?}\nS2 commit: {}", outcome(&second));
    let (disk, stored, same) = compare(&fixture, &ws, "S2", "docs");
    let after = ws.status().expect("status").changes;
    eprintln!("S2 status after the Commit: {after:?}");
    ws.discard().expect("discard");
    fixture.close();
    assert_eq!(stored, disk, "names below docs");
    assert!(same, "bytes below docs");
}

/// S3. The same swap, with the new directory moved in from outside the
/// Workspace (same volume, so it is a rename): unpack beside, move into
/// place.
#[test]
fn s3_a_directory_moved_in_from_outside_over_an_existing_name_reaches_the_commit() {
    let (fixture, ws) = swap_setup("rv2-dir-move-in");
    let root = ws.root().to_owned();
    let outside = fixture.home.path().join("unpacked");
    std::fs::create_dir_all(outside.join("sub")).expect("outside");
    std::fs::write(outside.join("guide.txt"), b"new guide").expect("file");
    std::fs::write(outside.join("sub").join("deep.txt"), b"deep").expect("file");
    // The Workspace has looked into docs, as any listing does.
    let _ = std::fs::read_dir(root.join("docs")).expect("list").count();
    ops::run(
        fixture.home.path(),
        &[
            Op::RemoveTree(root.join("docs")),
            Op::Rename(outside.clone(), root.join("docs")),
        ],
    );
    let status = ws.status().expect("status").changes;
    let done = ws.commit(Some("moved in"));
    eprintln!("S3 status: {status:?}\nS3 commit: {}", outcome(&done));
    let (disk, stored, same) = compare(&fixture, &ws, "S3", "docs");
    // A later change below the directory that arrived: is it tracked?
    ops::run(
        fixture.home.path(),
        &[Op::Write(at(&root, "docs/sub/deep.txt"), b"deeper".to_vec())],
    );
    let later = ws.commit(Some("later"));
    let tracked = fixture.saved("work").1.get("docs/sub/deep.txt") == Some(&digest(b"deeper"));
    eprintln!("S3 later commit: {}; later change committed: {tracked}", outcome(&later));
    ws.discard().expect("discard");
    fixture.close();
    assert_eq!(stored, disk, "names below docs");
    assert!(same, "bytes below docs");
    assert!(tracked, "a change below the moved-in directory was not committed");
}

const B2: &[u8] = b"# theirs\n";

/// S4. A file this Workspace rewrote with the SAME bytes (a formatter, a
/// generator, an editor's save) is a full file, captured in the very Commit
/// that merges another Workspace's change to it. The merge is clean: this
/// side did not change the content.
#[test]
fn s4_a_file_rewritten_unchanged_follows_the_merge_of_the_same_commit() {
    let fixture = Fixture::new("rv2-same-commit");
    let (a, b) = (fixture.mount("a"), fixture.mount("b"));
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(a.root().join("README.md"), b"# fixture\n".to_vec()),
            Op::Write(b.root().join("README.md"), B2.to_vec()),
        ],
    );
    b.commit(Some("theirs")).expect("b");
    ops::run(
        fixture.home.path(),
        &[Op::Write(a.root().join("other.txt"), b"other\n".to_vec())],
    );
    let merged = a.commit(Some("mine")).expect("a");
    assert!(merged.merged, "{merged:?}");
    assert_eq!(fixture.saved("work").1["README.md"], digest(B2));
    let disk = std::fs::read(a.root().join("README.md")).expect("read");
    let status = a.status().expect("status").changes;
    eprintln!(
        "S4 A's disk after the merge: {:?}; status: {status:?}; rebased: {:?}; warnings: {:?}",
        String::from_utf8_lossy(&disk),
        merged.rebased,
        merged.warnings
    );
    // The user edits what the directory shows and commits.
    let mut edited = disk.clone();
    edited.extend_from_slice(b"one more line\n");
    ops::run(
        fixture.home.path(),
        &[Op::Write(a.root().join("README.md"), edited)],
    );
    let again = a.commit(Some("edit"));
    let kept = fixture.saved("work").1["README.md"] == digest(b"# theirs\none more line\n");
    eprintln!(
        "S4 after an edit of what A showed: {}; the Branch still holds B's line: {kept}",
        outcome(&again)
    );
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
    assert_eq!(
        String::from_utf8_lossy(&disk),
        String::from_utf8_lossy(B2),
        "A's directory does not show the merged base, and status is clean"
    );
}

/// S5. A committed file inside a directory that is then renamed (and that
/// rename committed). Another Workspace changes the file; this one merges.
#[test]
fn s5_a_committed_file_below_a_renamed_directory_follows_a_later_merge() {
    let fixture = Fixture::new("rv2-ledger-rename");
    let a = fixture.mount("a");
    ops::run(
        fixture.home.path(),
        &[
            Op::Mkdir(a.root().join("pkg")),
            Op::Write(at(a.root(), "pkg/a.txt"), b"a1".to_vec()),
        ],
    );
    a.commit(Some("a1")).expect("a1");
    ops::run(
        fixture.home.path(),
        &[Op::Rename(a.root().join("pkg"), a.root().join("pkg2"))],
    );
    a.commit(Some("renamed")).expect("a2");
    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[Op::Write(at(b.root(), "pkg2/a.txt"), b"b2".to_vec())],
    );
    b.commit(Some("b2")).expect("b2");
    ops::run(
        fixture.home.path(),
        &[Op::Write(a.root().join("other.txt"), b"other".to_vec())],
    );
    let merged = a.commit(Some("a3")).expect("a3");
    assert!(merged.merged);
    assert_eq!(fixture.saved("work").1["pkg2/a.txt"], digest(b"b2"));
    let disk = std::fs::read(at(a.root(), "pkg2/a.txt")).expect("read");
    let status = a.status().expect("status").changes;
    eprintln!(
        "S5 A's disk after the merge: {:?}; status: {status:?}; rebased: {:?}",
        String::from_utf8_lossy(&disk),
        merged.rebased
    );
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
    assert_eq!(String::from_utf8_lossy(&disk), "b2");
}

/// S6. The record of committed files is called disposable ("without it
/// every full file is compared by its bytes once"). It is removed while the
/// Workspace is unmounted; the Branch does not move meanwhile.
#[test]
fn s6_without_the_record_a_committed_file_still_follows_a_merge() {
    let fixture = Fixture::new("rv2-ledger-lost");
    let a = fixture.mount("a");
    let (a_root, a_state) = (a.root().to_owned(), a.state().to_owned());
    ops::run(
        fixture.home.path(),
        &[Op::Write(at(&a_root, "src/main.rs"), b"fn main() { a1(); }\n".to_vec())],
    );
    a.commit(Some("a1")).expect("a1");
    a.unmount().expect("unmount");
    let record = a_state.join("committed");
    assert!(record.is_file(), "the record of committed files");
    std::fs::remove_file(&record).expect("remove");
    let (a, _) = Workspace::resume(&fixture.engine, &a_root, None, 0).expect("resume");
    assert!(a.status().expect("status").diverged.is_none());

    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[Op::Write(at(b.root(), "src/main.rs"), b"fn main() { b2(); }\n".to_vec())],
    );
    b.commit(Some("b2")).expect("b2");
    ops::run(
        fixture.home.path(),
        &[Op::Write(a_root.join("other.txt"), b"other".to_vec())],
    );
    let merged = a.commit(Some("a3")).expect("a3");
    assert!(merged.merged);
    let disk = std::fs::read(at(&a_root, "src/main.rs")).expect("read");
    let status = a.status().expect("status").changes;
    eprintln!(
        "S6 A's disk after the merge: {:?}; status: {status:?}; rebased: {:?}",
        String::from_utf8_lossy(&disk),
        merged.rebased
    );
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
    assert_eq!(String::from_utf8_lossy(&disk), "fn main() { b2(); }\n");
}

/// S7. Both Workspaces change the SAME file in different places. Whatever
/// the engine decides (a Conflict, or one merged file), the directory must
/// end up showing what the Branch holds, or say that it does not.
#[test]
fn s7_two_edits_of_one_file_in_different_places() {
    let fixture = Fixture::new("rv2-both-edit");
    let a = fixture.mount("a");
    let lines: Vec<String> = (1..=40).map(|n| format!("line {n}\n")).collect();
    let base = lines.concat();
    ops::run(
        fixture.home.path(),
        &[Op::Write(a.root().join("notes.txt"), base.clone().into_bytes())],
    );
    a.commit(Some("base")).expect("base");
    let b = fixture.mount("b");
    let mine = base.replacen("line 1\n", "line 1, edited by a\n", 1);
    let theirs = base.replacen("line 40\n", "line 40, edited by b\n", 1);
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(a.root().join("notes.txt"), mine.clone().into_bytes()),
            Op::Write(b.root().join("notes.txt"), theirs.into_bytes()),
        ],
    );
    b.commit(Some("b")).expect("b");
    let result = a.commit(Some("a"));
    let stored = fixture.saved("work").1["notes.txt"];
    let disk = std::fs::read(a.root().join("notes.txt")).expect("read");
    let status = a.status().expect("status").changes;
    eprintln!(
        "S7 commit: {}\nS7 disk equals the Branch: {}; disk is A's own version: {}; \
         disk has B's edit: {}; status: {status:?}",
        outcome(&result),
        digest(&disk) == stored,
        disk == mine.as_bytes(),
        String::from_utf8_lossy(&disk).contains("edited by b")
    );
    let published = result.is_ok();
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
    if published {
        assert_eq!(
            digest(&disk),
            stored,
            "the Commit published, and the directory does not show what the Branch holds"
        );
    }
}

fn forge(state: &std::path::Path, field: &str, value: &str) -> String {
    let anchor = state.join("anchor");
    let text = std::fs::read_to_string(&anchor).expect("anchor");
    let forged: String = text
        .lines()
        .map(|line| {
            if line.starts_with(&format!("{field}=")) {
                format!("{field}={value}\n")
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    assert_ne!(forged, text);
    std::fs::write(&anchor, forged).expect("anchor");
    text
}

/// S8. The recorded journal position lies AFTER the journal's end: the
/// volume was restored to an earlier image while the state directory (on
/// another volume, or restored separately) kept a later position. Records
/// "between" do not exist; nothing may be read as "no changes".
#[test]
fn s8_a_recorded_position_after_the_journals_end_is_not_trusted() {
    let fixture = Fixture::new("rv2-journal-future");
    let ws = fixture.mount("ws");
    let (root, state) = (ws.root().to_owned(), ws.state().to_owned());
    ops::run(
        fixture.home.path(),
        &[Op::Write(root.join("kept.txt"), b"kept".to_vec())],
    );
    ws.commit(Some("kept")).expect("commit");
    ws.unmount().expect("unmount");
    let before = forge(&state, "usn", "9000000000000000000");
    // A change while no provider runs: a full file can be rewritten.
    let down = ops::attempt(
        fixture.home.path(),
        &[Op::Write(root.join("kept.txt"), b"changed while down".to_vec())],
    );
    eprintln!("S8 rewrite of a full file while no provider runs: {down:?}");
    let (ws, found) = Workspace::resume(&fixture.engine, &root, None, 0).expect("resume");
    let first = ws.status().expect("status").changes;
    // And changes after the restart.
    ops::run(
        fixture.home.path(),
        &[Op::Write(root.join("after.txt"), b"after".to_vec())],
    );
    std::thread::sleep(Duration::from_millis(500));
    let second = ws.status().expect("status").changes;
    let done = ws.commit(Some("after"));
    let (saved_shape, saved_digests) = fixture.saved("work");
    let anchor_now = std::fs::read_to_string(state.join("anchor")).unwrap_or_default();
    eprintln!(
        "S8 anchor before forging: {}\nS8 resume found {found}; status at resume: {first:?}; \
         status after a new file: {second:?}; commit: {}\nS8 anchor now: {}",
        before.replace('\n', " "),
        outcome(&done),
        anchor_now.replace('\n', " ")
    );
    let kept = std::fs::read(root.join("kept.txt")).expect("read");
    eprintln!(
        "S8 kept.txt committed as on disk: {}; after.txt committed: {}",
        saved_digests.get("kept.txt") == Some(&digest(&kept)),
        saved_shape.contains_key("after.txt")
    );
    let ok = saved_digests.get("kept.txt") == Some(&digest(&kept))
        && saved_shape.contains_key("after.txt");
    ws.discard().expect("discard");
    fixture.close();
    assert!(ok, "changes were missed after a journal position that lies in the future");
}

/// S9. The recorded position lies before the journal's oldest record (the
/// journal wrapped while the Workspace was unmounted), and the ONE change
/// made meanwhile kept the file's size. The fallback must compare, and the
/// record of committed files must not vouch for the changed file.
#[test]
fn s9_after_a_wrapped_journal_a_same_size_change_is_found() {
    let fixture = Fixture::new("rv2-journal-wrapped");
    let ws = fixture.mount("ws");
    let (root, state) = (ws.root().to_owned(), ws.state().to_owned());
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(root.join("kept.txt"), b"kept".to_vec()),
            Op::Write(root.join("same.txt"), b"aaaa".to_vec()),
        ],
    );
    ws.commit(Some("kept")).expect("commit");
    ws.unmount().expect("unmount");
    forge(&state, "usn", "8");
    let down = ops::attempt(
        fixture.home.path(),
        &[Op::Write(root.join("same.txt"), b"bbbb".to_vec())],
    );
    eprintln!("S9 rewrite of a full file while no provider runs: {down:?}");
    let (ws, found) = Workspace::resume(&fixture.engine, &root, None, 0).expect("resume");
    let status = ws.status().expect("status").changes;
    let done = ws.commit(Some("after"));
    let stored = fixture.saved("work").1["same.txt"];
    let on_disk = std::fs::read(root.join("same.txt")).expect("read");
    eprintln!(
        "S9 resume found {found}; status: {status:?}; commit: {}",
        outcome(&done)
    );
    ws.discard().expect("discard");
    fixture.close();
    assert_eq!(on_disk, b"bbbb", "the change while down did not happen");
    assert_eq!(stored, digest(b"bbbb"));
}

/// S10. A view kept mapped after its file handle and mapping handle were
/// closed, then written. Prints what status and Commit see; the gap while a
/// file is open is documented by the author, this is the closed-handle case.
#[test]
fn s10_a_store_through_a_view_kept_after_the_handles_closed() {
    let fixture = Fixture::new("rv2-view-after-close");
    let ws = fixture.mount("ws");
    let file = ws.root().join("data.bin");
    ops::run(
        fixture.home.path(),
        &[Op::Write(file.clone(), vec![b'a'; 8192])],
    );
    ws.commit(None).expect("commit 1");
    let (closed, go, wrote) = (
        fixture.home.path().join("closed"),
        fixture.home.path().join("go"),
        fixture.home.path().join("wrote"),
    );
    let mut mapper = powershell(&format!(
        "$s=[System.IO.File]::Open('{}','Open','ReadWrite','ReadWrite,Delete'); \
         $f=[System.IO.MemoryMappedFiles.MemoryMappedFile]::CreateFromFile($s,[NullString]::Value,0,\
         [System.IO.MemoryMappedFiles.MemoryMappedFileAccess]::ReadWrite,$null,\
         [System.IO.HandleInheritability]::None,$false); $a=$f.CreateViewAccessor(); \
         $f.Dispose(); $s.Dispose(); Set-Content '{}' 'closed'; \
         while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 20 }}; \
         $a.Write(0,[byte]88); $a.Flush(); Set-Content '{}' 'wrote'; \
         Start-Sleep -Milliseconds 4000; $a.Dispose()",
        file.display(),
        closed.display(),
        go.display(),
        wrote.display()
    ));
    wait_for("the handles are closed", || closed.exists());
    std::thread::sleep(Duration::from_millis(500));
    // Whatever opening the file recorded is committed away first.
    let settle = ws.commit(None);
    std::fs::write(&go, b"go").expect("signal");
    wait_for("the view is written", || wrote.exists());
    std::thread::sleep(Duration::from_millis(500));
    let mut expected = vec![b'a'; 8192];
    expected[0] = 88;
    let on_disk = std::fs::read(&file).expect("read") == expected;
    let while_mapped = ws.status().expect("status").changes;
    let during = ws.commit(None);
    let stored_during = fixture.saved("work").1["data.bin"] == digest(&expected);
    assert!(mapper.wait().expect("mapper").success());
    std::thread::sleep(Duration::from_millis(1000));
    let after_unmap = ws.status().expect("status").changes;
    let after = ws.commit(None);
    let stored_after = fixture.saved("work").1["data.bin"] == digest(&expected);
    eprintln!(
        "S10 settle commit: {}\nS10 the store is on disk: {on_disk}\n\
         S10 while the view is mapped: status {while_mapped:?}; commit {}; in Store: {stored_during}\n\
         S10 after the view is unmapped: status {after_unmap:?}; commit {}; in Store: {stored_after}",
        outcome(&settle),
        outcome(&during),
        outcome(&after)
    );
    ws.discard().expect("discard");
    fixture.close();
    assert!(on_disk, "the mapped store did not reach the file");
    assert!(
        stored_after,
        "a store through a view kept after its handles closed is never committed"
    );
}
