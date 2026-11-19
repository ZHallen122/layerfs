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
