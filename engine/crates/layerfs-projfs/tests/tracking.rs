//! Change tracking from the volume's change journal and the record of
//! committed files: the cases the review's repro tests do not reach.
#![cfg(windows)]

mod support;

use layerfs_projfs::workspace::{CommitRefusal, Outcome};
use layerfs_projfs::{Change, Kind, Workspace};
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

fn change(path: &str, kind: Kind, directory: bool) -> Change {
    Change {
        path: path.to_owned(),
        kind,
        directory,
    }
}

fn powershell(script: &str) -> std::process::Child {
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdout(Stdio::null())
        .spawn()
        .expect("powershell")
}

/// The change journal records a write through a handle once: later writes
/// through the same handle add nothing until it closes. A file whose handle
/// the journal has not shown closed is therefore compared at every Commit.
#[test]
fn every_write_through_one_open_handle_reaches_the_commit_after_it() {
    let fixture = Fixture::new("tk-open-handle");
    let ws = fixture.mount("ws");
    let file = ws.root().join("log.txt");
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
         $s.WriteByte(66); $s.Flush(); Start-Sleep -Milliseconds 6000; $s.Dispose()",
        file.display(),
        go.display()
    ));
    let starts = |prefix: &[u8]| std::fs::read(&file).is_ok_and(|bytes| bytes.starts_with(prefix));
    wait_for("the first write", || starts(b"A"));
    assert_eq!(
        ws.status().expect("status").changes,
        vec![change("log.txt", Kind::Modified, false)]
    );
    let second = ws.commit(None).expect("commit 2");
    assert!(matches!(second.outcome, Outcome::Committed { .. }));
    assert_eq!(fixture.saved("work").1["log.txt"], digest(b"A123456789\n"));

    std::fs::write(&go, b"go").expect("signal");
    wait_for("the second write", || starts(b"AB"));
    let third = ws.commit(None).expect("commit 3");
    let still_open = writer.try_wait().expect("child").is_none();
    let committed = fixture.saved("work").1["log.txt"];
    writer.wait().expect("child");
    assert!(still_open, "the writer closed before the Commit; rerun");
    assert!(matches!(third.outcome, Outcome::Committed { .. }));
    assert_eq!(committed, digest(b"AB23456789\n"));
    // Once the handle is closed and nothing changed, the file is settled.
    wait_for("the close reaches the journal", || {
        ws.commit(None)
            .is_ok_and(|done| matches!(done.outcome, Outcome::UpToDate { .. }))
    });
    assert_eq!(ws.status().expect("status").changes, Vec::new());
    ws.discard().expect("discard");
    fixture.close();
}

/// A name deleted here and committed leaves a tombstone on disk. When
/// another Workspace brings the name back, the tombstone must not hide it.
#[test]
fn a_name_deleted_here_and_added_again_elsewhere_follows_a_merge() {
    let fixture = Fixture::new("tk-tombstone");
    let a = fixture.mount("a");
    ops::run(
        fixture.home.path(),
        &[
            Op::Remove(a.root().join("empty.bin")),
            Op::Remove(a.root().join("README.md")),
        ],
    );
    a.commit(Some("deleted")).expect("a1");
    assert!(!fixture.saved("work").0.contains_key("empty.bin"));

    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[Op::Write(b.root().join("empty.bin"), b"back".to_vec())],
    );
    b.commit(Some("back")).expect("b2");

    ops::run(
        fixture.home.path(),
        &[Op::Write(a.root().join("other.txt"), b"other".to_vec())],
    );
    let merged = a.commit(Some("a3")).expect("a3");
    assert!(merged.merged);
    assert_eq!(
        std::fs::read(a.root().join("empty.bin")).expect("the name is back"),
        b"back"
    );
    // The name nobody brought back stays deleted.
    assert!(!a.root().join("README.md").exists());
    assert_eq!(a.status().expect("status").changes, Vec::new());
    let (saved, _) = fixture.saved("work");
    assert_eq!(shape(a.root()), saved);
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
}

/// A restart after the Branch moved: the engine can bind only at the
/// present head, so the merge base is gone. The directory keeps showing
/// the recorded base, status is told against it, and Commit is refused.
#[test]
fn a_restart_after_the_branch_moved_refuses_commit_and_says_why() {
    let fixture = Fixture::new("tk-diverged");
    let a = fixture.mount("a");
    let a_root = a.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[Op::Write(a_root.join("mine.txt"), b"mine".to_vec())],
    );
    let before = tree_digests(&a_root);
    a.unmount().expect("unmount");

    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[Op::Write(
            b.root().join("README.md"),
            b"# theirs\n".to_vec(),
        )],
    );
    b.commit(Some("theirs")).expect("b");
    b.discard().expect("discard");
    let head = fixture.head();

    let (a, found) = Workspace::resume(&fixture.engine, &a_root, None, 0).expect("resume");
    assert_eq!(found, 1);
    let status = a.status().expect("status");
    assert!(status.diverged.is_some());
    assert_eq!(status.changes, vec![change("mine.txt", Kind::Added, false)]);
    // The directory is what it was: the recorded base and the local change.
    assert_eq!(tree_digests(&a_root), before);
    match a.commit(None) {
        Err(CommitRefusal::Diverged { .. }) => {}
        other => panic!("expected the Commit to be refused as diverged: {other:?}"),
    }
    assert_eq!(fixture.head(), head, "nothing was published");
    a.discard().expect("discard");
    fixture.close();
}

/// When the recorded journal position no longer means anything (another
/// journal, or records already overwritten), everything on disk is
/// compared once and nothing is missed.
#[test]
fn when_the_change_journal_cannot_answer_everything_on_disk_is_compared() {
    let fixture = Fixture::new("tk-journal-lost");
    let ws = fixture.mount("ws");
    let (root, state) = (ws.root().to_owned(), ws.state().to_owned());
    // One committed file that stays as committed, so the record of
    // committed files has something to vouch for.
    ops::run(
        fixture.home.path(),
        &[Op::Write(root.join("kept.txt"), b"kept".to_vec())],
    );
    ws.commit(Some("kept")).expect("commit");
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(
                at(&root, "src/main.rs"),
                b"fn main() { changed(); }\n".to_vec(),
            ),
            Op::Write(root.join("new.txt"), b"new".to_vec()),
            Op::Remove(root.join("empty.bin")),
            Op::ReadOnly(root.join("README.md"), true),
            Op::Mkdir(root.join("made")),
            Op::Write(at(&root, "made/inside.txt"), b"inside".to_vec()),
            // A file read and never changed: it holds its bytes on disk,
            // and they are the base's.
            Op::Read(at(&root, "assets/large.bin")),
        ],
    );
    let expected = vec![
        change("README.md", Kind::Modified, false),
        change("empty.bin", Kind::Deleted, false),
        change("made", Kind::Added, true),
        change("made/inside.txt", Kind::Added, false),
        change("new.txt", Kind::Added, false),
        change("src/main.rs", Kind::Modified, false),
    ];
    assert_eq!(ws.status().expect("status").changes, expected);
    ws.unmount().expect("unmount");

    // The state now names a journal that does not exist.
    let anchor = state.join("anchor");
    let text = std::fs::read_to_string(&anchor).expect("anchor");
    let forged: String = text
        .lines()
        .map(|line| {
            if line.starts_with("journal=") {
                "journal=1\n".to_owned()
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    assert_ne!(forged, text);
    std::fs::write(&anchor, forged).expect("anchor");

    let (ws, found) = Workspace::resume(&fixture.engine, &root, None, 0).expect("resume");
    assert_eq!(found, expected.len());
    assert_eq!(ws.status().expect("status").changes, expected);
    let done = ws
        .commit(Some("after the journal was lost"))
        .expect("commit");
    assert!(matches!(done.outcome, Outcome::Committed { .. }));
    // "Everything" is every full file, every name and every placeholder's
    // facts. A placeholder whose size and time are the base's is not read:
    // neither the megabyte that is on disk, nor a file that is not.
    assert!(
        done.captured.bytes_read < 1024,
        "the Commit read {} bytes",
        done.captured.bytes_read
    );
    assert_eq!(ws.projection().provider().served().reads, 0);
    let (saved_shape, saved_digests) = fixture.saved("work");
    assert_eq!(saved_shape, shape(&root));
    assert_eq!(saved_digests, tree_digests(&root));
    assert_eq!(ws.status().expect("status").changes, Vec::new());
    ws.discard().expect("discard");
    fixture.close();
}

/// A junction is never followed: not when it is met as a changed item, and
/// not when a new directory that holds one is taken in whole. It is named.
#[test]
fn a_junction_is_named_and_never_followed() {
    let fixture = Fixture::new("tk-junction");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    let outside = fixture.home.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(outside.join("secret.txt"), b"not the Workspace's").expect("file");
    ops::run(
        fixture.home.path(),
        &[
            Op::Mkdir(root.join("vendor")),
            Op::Write(at(&root, "vendor/real.txt"), b"real".to_vec()),
        ],
    );
    for link in [root.join("linked"), at(&root, "vendor/linked")] {
        let made = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .stdout(Stdio::null())
            .status()
            .expect("mklink");
        assert!(made.success(), "the junction could not be created");
    }
    let status = ws.status().expect("status");
    assert_eq!(
        status.changes,
        vec![
            change("vendor", Kind::Added, true),
            change("vendor/real.txt", Kind::Added, false),
        ]
    );
    let named: Vec<&str> = status
        .hidden
        .iter()
        .filter(|(_, reason)| reason.contains("junction"))
        .map(|(path, _)| path.as_str())
        .collect();
    assert_eq!(named, ["linked", "vendor/linked"]);

    let done = ws.commit(None).expect("commit");
    let mut links = done.captured.links.clone();
    links.sort();
    assert_eq!(links, ["linked", "vendor/linked"]);
    let (saved, _) = fixture.saved("work");
    let stored: Vec<&String> = saved
        .keys()
        .filter(|path| path.starts_with("vendor") || path.starts_with("linked"))
        .collect();
    assert_eq!(stored, ["vendor", "vendor/real.txt"]);
    ws.discard().expect("discard");
    assert!(
        outside.join("secret.txt").exists(),
        "the target was touched"
    );
    fixture.close();
}

/// A committed file changed only through a mapped view. The view's stores
/// move neither the file's size nor its time, and NTFS writes no change
/// journal record for them while the file stays open. So the change is NOT
/// seen while the file is mapped: that is the gap this test documents. Once
/// the file is closed the journal has the record, and the change is found
/// although size and time are those of the committed file.
#[test]
fn a_store_through_a_mapped_view_is_seen_once_the_file_is_closed() {
    let fixture = Fixture::new("tk-mapped");
    let ws = fixture.mount("ws");
    let file = ws.root().join("data.bin");
    ops::run(
        fixture.home.path(),
        &[Op::Write(file.clone(), vec![b'a'; 8192])],
    );
    ws.commit(None).expect("commit 1");
    let before = std::fs::metadata(&file).expect("metadata");
    let marker = fixture.home.path().join("mapped");
    let mut mapper = powershell(&format!(
        "$s=[System.IO.File]::Open('{}','Open','ReadWrite','ReadWrite');          $f=[System.IO.MemoryMappedFiles.MemoryMappedFile]::CreateFromFile($s,[NullString]::Value,0,         [System.IO.MemoryMappedFiles.MemoryMappedFileAccess]::ReadWrite,$null,         [System.IO.HandleInheritability]::None,$false); $a=$f.CreateViewAccessor();          $a.Write(0,[byte]88); Set-Content '{}' 'mapped';          Start-Sleep -Milliseconds 1500; $a.Dispose(); $f.Dispose(); $s.Dispose()",
        file.display(),
        marker.display()
    ));
    wait_for("the view is written", || marker.exists());
    let mut expected = vec![b'a'; 8192];
    expected[0] = 88;
    assert_eq!(std::fs::read(&file).expect("read"), expected);
    let while_mapped = ws.status().expect("status").changes;
    assert!(mapper.wait().expect("mapper").success());

    wait_for("the close reaches the journal", || {
        !ws.status().expect("status").changes.is_empty()
    });
    assert_eq!(
        ws.status().expect("status").changes,
        vec![change("data.bin", Kind::Modified, false)]
    );
    let after = std::fs::metadata(&file).expect("metadata");
    let done = ws.commit(None).expect("commit 2");
    assert!(matches!(done.outcome, Outcome::Committed { .. }));
    assert_eq!(fixture.saved("work").1["data.bin"], digest(&expected));
    ws.discard().expect("discard");
    fixture.close();
    eprintln!(
        "while the file was mapped, status showed {while_mapped:?}; size and time moved: {}",
        (before.len(), before.modified().ok()) != (after.len(), after.modified().ok())
    );
}

/// Discard after the kinds of change that leave unusual items on disk: a
/// placeholder made read-only without ever being read, a placeholder
/// renamed without ever being read, deleted names, and a local directory.
#[test]
fn discard_removes_every_kind_of_item_a_session_leaves() {
    let fixture = Fixture::new("tk-discard");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[
            Op::Read(root.join("README.md")),
            Op::ReadOnly(at(&root, "src/main.rs"), true),
            Op::Rename(
                at(&root, "locked/read-only.txt"),
                at(&root, "locked/moved.txt"),
            ),
            Op::Remove(root.join("empty.bin")),
            Op::Rename(root.join("README.md"), root.join("README2.md")),
            Op::Mkdir(root.join("pkg")),
            Op::Write(at(&root, "pkg/a.txt"), b"one".to_vec()),
            Op::Rename(root.join("pkg"), root.join("pkg2")),
        ],
    );
    assert_eq!(ws.status().expect("status").changes.len(), 6);
    ws.discard().expect("discard");
    assert!(!root.exists(), "the Workspace directory is gone");
    fixture.close();
}

fn icacls(file: &std::path::Path, arguments: &[&str]) {
    let done = Command::new("icacls")
        .arg(file)
        .args(arguments)
        .stdout(Stdio::null())
        .status()
        .expect("icacls");
    assert!(done.success(), "icacls {arguments:?}");
}

/// A changed file that is on the volume and may not be opened is not taken
/// for a file that is gone: nobody knows what it holds. Status and Commit
/// say so and publish nothing, until it can be opened again.
#[test]
fn a_changed_file_that_may_not_be_opened_refuses_the_commit() {
    let fixture = Fixture::new("tk-denied");
    let ws = fixture.mount("ws");
    let file = ws.root().join("guarded.txt");
    ops::run(
        fixture.home.path(),
        &[Op::Write(file.clone(), b"one".to_vec())],
    );
    ws.commit(Some("one")).expect("commit 1");
    ops::run(
        fixture.home.path(),
        &[Op::Write(file.clone(), b"two".to_vec())],
    );
    // Everyone is denied everything on the file itself.
    icacls(&file, &["/deny", "*S-1-1-0:(F)"]);
    let status = ws.status();
    let refused = ws.commit(Some("two"));
    icacls(&file, &["/remove:d", "*S-1-1-0"]);
    let text = |error: &dyn std::fmt::Display| error.to_string();
    let status = status.as_ref().map(|_| ()).map_err(|e| text(e));
    let refused = refused.as_ref().map(|_| ()).map_err(|e| text(e));
    eprintln!("denied: status {status:?}; commit {refused:?}");
    assert_eq!(
        fixture.saved("work").1["guarded.txt"],
        digest(b"one"),
        "nothing was published while the file could not be opened"
    );
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("cannot be opened")),
        "{refused:?}"
    );
    assert!(status.is_err(), "{status:?}");
    // Nothing was dropped: the change is committed once the file opens.
    ws.commit(Some("two")).expect("commit 2");
    assert_eq!(fixture.saved("work").1["guarded.txt"], digest(b"two"));
    ws.discard().expect("discard");
    fixture.close();
}

/// A write to a projected file that has returned is in a Commit begun right
/// after it. NTFS records such a write while the file is still marked a
/// placeholder; ProjFS takes the mark off a moment later. A tracker that
/// waits for the mark to go misses the write in that moment (it did: about
/// one run in five of another test). The moment cannot be forced, so the
/// case is run on twenty fresh mounts, four kinds of write each, with the
/// last write of each round the likeliest to be caught in it.
#[test]
fn a_write_to_a_projected_file_is_in_the_commit_begun_right_after_it() {
    let fixture = Fixture::new("tk-fresh-write");
    let launch = "docs/\u{1f680}-launch.txt";
    for round in 0..20usize {
        let ws = fixture.mount(&format!("ws{round}"));
        let root = ws.root().to_owned();
        // Read first, so two of the files hold their bytes already.
        ops::run(
            fixture.home.path(),
            &[
                Op::Read(root.join("README.md")),
                Op::Read(at(&root, launch)),
            ],
        );
        let quiet = ws.commit(None).expect("commit");
        assert!(
            matches!(quiet.outcome, Outcome::UpToDate { .. }) && quiet.captured.bytes_read == 0,
            "reading a file is not a change: {quiet:?}"
        );
        let stamp = format!("round {round}\n").into_bytes();
        let mut writes = vec![
            // replaced whole, never read before
            Op::Write(at(&root, "src/main.rs"), stamp.clone()),
            // replaced whole, read before
            Op::Write(root.join("README.md"), stamp.clone()),
            // appended to, read before
            Op::Append(at(&root, launch), stamp.clone()),
            // overwritten in place from its start, never read before
            Op::Hold(at(&root, "src/lib/deep/mod.rs"), stamp.clone(), 0),
        ];
        writes.rotate_left(round % 4);
        ops::run(fixture.home.path(), &writes);
        let done = ws.commit(Some("right after")).expect("commit");
        let (_, saved) = fixture.saved("work");
        for path in ["src/main.rs", "README.md", launch, "src/lib/deep/mod.rs"] {
            let on_disk = std::fs::read(at(&root, path)).expect("read");
            assert_eq!(
                saved.get(path),
                Some(&digest(&on_disk)),
                "round {round}: {path} is not in the Commit made right after its write: {done:?}"
            );
        }
        ws.discard().expect("discard");
    }
    fixture.close();
}

/// Starts a process that opens `paths` for reading, allowing others to write
/// them, reads the one at `read` to its end, writes the file `ready`, and
/// keeps them all open until the file `go` exists.
fn reader(
    paths: &[std::path::PathBuf],
    read: Option<usize>,
    ready: &std::path::Path,
    go: &std::path::Path,
) -> std::process::Child {
    let mut opens = String::new();
    for path in paths {
        opens.push_str(&format!(
            "[System.IO.File]::Open('{}','Open','Read','ReadWrite,Delete');",
            path.display()
        ));
    }
    let read = read.map_or_else(String::new, |index| {
        format!("while ($held[{index}].Read($b,0,65536) -gt 0) {{}};")
    });
    powershell(&format!(
        "$held=@({opens}); $b=New-Object byte[] 65536; {read} Set-Content '{}' 'ready'; \
         while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 20 }}; \
         $held | ForEach-Object {{ $_.Dispose() }}",
        ready.display(),
        go.display()
    ))
}

/// The same moment, held open: while another process keeps a projected file
/// open for reading, a third writes it and exits. The write is on disk and
/// its writer is gone; whether ProjFS has taken the placeholder mark off by
/// then is its own business. The Commit must have the bytes: also of a file
/// overwritten in place, for which the journal has nothing but the record
/// of ProjFS writing its content in.
#[test]
fn a_write_to_a_projected_file_a_reader_holds_open_is_in_the_next_commit() {
    let fixture = Fixture::new("tk-held-reader");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    let files = [
        "README.md",
        "src/main.rs",
        "assets/large.bin",
        "src/lib/deep/mod.rs",
    ];
    let (ready, go) = (
        fixture.home.path().join("ready"),
        fixture.home.path().join("go"),
    );
    // One of the files is read to its end first, so it holds its bytes
    // before it is written; the others are only opened.
    let paths: Vec<_> = files.iter().map(|path| at(&root, path)).collect();
    let mut reader = reader(&paths, Some(2), &ready, &go);
    wait_for("the reader holds the files", || ready.exists());
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(
                root.join("README.md"),
                b"# written under a reader\n".to_vec(),
            ),
            Op::Append(at(&root, "src/main.rs"), b"// appended\n".to_vec()),
            // Overwritten in place, the length unchanged: once a file that
            // holds its bytes, once one that does not yet.
            Op::Hold(at(&root, "assets/large.bin"), b"in place".to_vec(), 0),
            Op::Hold(at(&root, "src/lib/deep/mod.rs"), b"in place".to_vec(), 0),
        ],
    );
    let status = ws.status().expect("status").changes;
    let done = ws.commit(Some("under a reader"));
    let still_held = reader.try_wait().expect("reader").is_none();
    let (_, saved) = fixture.saved("work");
    std::fs::write(&go, b"go").expect("signal");
    reader.wait().expect("reader");
    eprintln!(
        "held reader: status {status:?}; commit {:?}",
        done.as_ref().map(|d| &d.outcome)
    );
    assert!(still_held, "the reader ended before the Commit; rerun");
    for path in files {
        assert_eq!(
            saved.get(path),
            Some(&digest(&std::fs::read(at(&root, path)).expect("read"))),
            "{path} is not in the Commit made after its write"
        );
    }
    assert_eq!(status.len(), files.len(), "{status:?}");
    ws.discard().expect("discard");
    fixture.close();
}

/// A reader keeps a committed file open while one process after another
/// overwrites it in place. The first write is a journal record. The file's
/// changes are not closed while the reader holds it, so the second write
/// adds none; the file has to stay suspect, though no writer has it open
/// when the first Commit reads it.
#[test]
fn a_second_write_under_a_reader_is_in_the_commit_after_it() {
    let fixture = Fixture::new("tk-reader-two-writes");
    let ws = fixture.mount("ws");
    let file = ws.root().join("data.txt");
    ops::run(
        fixture.home.path(),
        &[Op::Write(file.clone(), b"0123456789".to_vec())],
    );
    ws.commit(Some("committed")).expect("commit 1");
    let (ready, go) = (
        fixture.home.path().join("ready"),
        fixture.home.path().join("go"),
    );
    let mut reader = reader(&[file.clone()], None, &ready, &go);
    wait_for("the reader holds the file", || ready.exists());
    let stored = |expected: &[u8]| fixture.saved("work").1["data.txt"] == digest(expected);

    ops::run(
        fixture.home.path(),
        &[Op::Hold(file.clone(), b"AB".to_vec(), 0)],
    );
    let second = ws.commit(Some("first write")).expect("commit 2");
    assert!(stored(b"AB23456789"), "{second:?}");
    assert!(
        second.captured.live.is_empty(),
        "no writer has the file open: {second:?}"
    );

    ops::run(
        fixture.home.path(),
        &[Op::Hold(file.clone(), b"CD".to_vec(), 0)],
    );
    let status = ws.status().expect("status").changes;
    let third = ws.commit(Some("second write")).expect("commit 3");
    let still_held = reader.try_wait().expect("reader").is_none();
    let has_second = stored(b"CD23456789");
    std::fs::write(&go, b"go").expect("signal");
    reader.wait().expect("reader");
    assert!(still_held, "the reader ended before the Commit; rerun");
    assert!(
        has_second,
        "the second write is not in the Commit after it: status {status:?}, {third:?}"
    );
    // Once the reader is gone the file's changes are closed, and the file
    // stops being looked at: a Commit with nothing changed reads nothing.
    wait_for("the close reaches the journal", || {
        ws.commit(None)
            .is_ok_and(|done| done.captured.bytes_read == 0)
    });
    ws.discard().expect("discard");
    fixture.close();
}

/// The record of committed files is a shortcut, and this checks that it
/// still is one: after the journal's records were overwritten, everything
/// on disk is looked at, and a committed file that is exactly as its Commit
/// read it is vouched for and not read again.
#[test]
fn after_overwritten_journal_records_a_committed_file_is_not_read_again() {
    let fixture = Fixture::new("tk-vouched");
    let ws = fixture.mount("ws");
    let (root, state) = (ws.root().to_owned(), ws.state().to_owned());
    ops::run(
        fixture.home.path(),
        &[Op::Write(root.join("big.bin"), vec![7u8; 200_000])],
    );
    ws.commit(Some("big")).expect("commit");
    ws.unmount().expect("unmount");
    let anchor = state.join("anchor");
    let text = std::fs::read_to_string(&anchor).expect("anchor");
    let forged: String = text
        .lines()
        .map(|line| match line.starts_with("usn=") {
            true => "usn=8\n".to_owned(),
            false => format!("{line}\n"),
        })
        .collect();
    assert_ne!(forged, text);
    std::fs::write(&anchor, forged).expect("anchor");
    let (ws, found) = Workspace::resume(&fixture.engine, &root, None, 0).expect("resume");
    assert_eq!(found, 0);
    let done = ws.commit(None).expect("commit");
    assert!(
        matches!(done.outcome, Outcome::UpToDate { .. }) && done.captured.bytes_read == 0,
        "{done:?}"
    );
    ws.discard().expect("discard");
    fixture.close();
}
