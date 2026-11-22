//! Third-round review attacks (local scratch branch only; not part of the
//! reviewed branch): the rules of d78023836. A file stays suspect while its
//! journal changes are not closed, and is trusted (entered in the record of
//! committed files) only when its newest journal record is a close.
#![cfg(windows)]

mod support;

use layerfs_projfs::workspace::{CommitRefusal, Committed, Outcome};
use layerfs_projfs::Workspace;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use support::fixture::Fixture;
use support::ops::{self, Op};

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

fn outcome(result: &Result<Committed, CommitRefusal>) -> String {
    match result {
        Ok(done) => format!(
            "{:?}; live {:?}; held {}; rebased {:?}; warnings {:?}",
            done.outcome,
            done.captured.live,
            done.captured.held.len(),
            done.rebased,
            done.warnings
        ),
        Err(refusal) => refusal.to_string(),
    }
}

/// T1. A reader holds a committed file open the whole time, so the journal
/// shows no close. A first writer writes and closes; Commit. The provider
/// restarts. A second writer overwrites in place and closes: with the
/// reader still there, that write adds no journal record at all. The file
/// must still be suspect after the restart.
#[test]
fn t1_a_second_write_under_a_reader_after_a_restart_reaches_the_commit() {
    let fixture = Fixture::new("rv3-reader-restart");
    let ws = fixture.mount("ws");
    let root = ws.root().to_owned();
    let file = root.join("kept.txt");
    ops::run(
        fixture.home.path(),
        &[Op::Write(file.clone(), b"0123456789\n".to_vec())],
    );
    ws.commit(None).expect("commit 1");
    let (ready, done) = (
        fixture.home.path().join("reader-ready"),
        fixture.home.path().join("reader-done"),
    );
    let mut reader = powershell(&format!(
        "$s=[System.IO.File]::Open('{}','Open','Read','ReadWrite,Delete'); \
         Set-Content '{}' 'ready'; \
         while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 20 }}; $s.Dispose()",
        file.display(),
        ready.display(),
        done.display()
    ));
    wait_for("the reader holds the file", || ready.exists());
    ops::run(
        fixture.home.path(),
        &[Op::Hold(file.clone(), b"A".to_vec(), 0)],
    );
    let second = ws.commit(None);
    eprintln!("T1 commit 2: {}", outcome(&second));
    assert_eq!(fixture.saved("work").1["kept.txt"], digest(b"A123456789\n"));

    ws.unmount().expect("unmount");
    let (ws, found) = Workspace::resume(&fixture.engine, &root, None, 0).expect("resume");
    ops::run(
        fixture.home.path(),
        &[Op::Hold(file.clone(), b"AB".to_vec(), 0)],
    );
    let status = ws.status().expect("status").changes;
    let third = ws.commit(None);
    let held = reader.try_wait().expect("reader").is_none();
    let committed = fixture.saved("work").1["kept.txt"];
    eprintln!(
        "T1 resume found {found}; status after the second write: {status:?}; commit 3: {}",
        outcome(&third)
    );
    std::fs::write(&done, b"done").expect("signal");
    reader.wait().expect("reader");
    // Once the reader is gone the file settles and stops being read.
    std::thread::sleep(Duration::from_millis(600));
    let fourth = ws.commit(None);
    let fifth = ws.commit(None);
    eprintln!(
        "T1 after the reader closed: {}\nT1 and once more: bytes read {}",
        outcome(&fourth),
        fifth.as_ref().map_or(0, |done| done.captured.bytes_read)
    );
    ws.discard().expect("discard");
    fixture.close();
    assert!(held, "the reader closed early; rerun");
    assert_eq!(
        committed,
        digest(b"AB23456789\n"),
        "the write made under the reader after the restart was not committed"
    );
}

/// T2. Trust against a merge. A committed file is in the record of
/// committed files. A process maps it writable, closes its handles and
/// stores a byte through the view without flushing. Another Workspace
/// changes the same file and commits; this one merges. The mapped store
/// must not vanish without a word: the file keeps it, or the Commit names
/// the file (a Conflict, a warning, "kept" or "refused").
#[test]
fn t2_a_mapped_store_is_not_replaced_silently_by_a_merge() {
    let fixture = Fixture::new("rv3-mapped-merge");
    let a = fixture.mount("a");
    let file = a.root().join("data.bin");
    ops::run(
        fixture.home.path(),
        &[Op::Write(file.clone(), vec![b'a'; 8192])],
    );
    a.commit(Some("a1")).expect("a1");
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
         $a.Write(0,[byte]88); Set-Content '{}' 'wrote'; \
         Start-Sleep -Milliseconds 5000; $a.Dispose()",
        file.display(),
        closed.display(),
        go.display(),
        wrote.display()
    ));
    wait_for("the handles are closed", || closed.exists());
    std::thread::sleep(Duration::from_millis(500));
    // Whatever opening the file recorded is committed away first.
    let settle = a.commit(None);
    eprintln!("T2 settle commit: {}", outcome(&settle));

    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[Op::Write(b.root().join("data.bin"), vec![b'b'; 8192])],
    );
    b.commit(Some("b2")).expect("b2");

    std::fs::write(&go, b"go").expect("signal");
    wait_for("the view is written", || wrote.exists());
    ops::run(
        fixture.home.path(),
        &[Op::Write(a.root().join("other.txt"), b"other".to_vec())],
    );
    let status = a.status().expect("status").changes;
    let merged = a.commit(Some("a3"));
    eprintln!(
        "T2 status before the merge: {status:?}\nT2 merge commit: {}",
        outcome(&merged)
    );
    let mapped_still = mapper.try_wait().expect("mapper").is_none();
    assert!(mapper.wait().expect("mapper").success());
    std::thread::sleep(Duration::from_millis(500));
    let disk = std::fs::read(&file).expect("read");
    let after = a.status().expect("status").changes;
    let branch = fixture.saved("work").1["data.bin"];
    eprintln!(
        "T2 view still mapped at the merge: {mapped_still}; after unmap the file starts with {:?}, \
         is all b: {}; status: {after:?}; the Branch holds B's: {}",
        disk.first().map(|byte| *byte as char),
        disk.iter().all(|byte| *byte == b'b'),
        branch == digest(&[b'b'; 8192])
    );
    let named = match &merged {
        Err(refusal) => refusal.to_string().contains("Conflict"),
        Ok(done) => {
            let text = format!("{:?}{:?}", done.rebased, done.warnings);
            text.contains("data.bin")
        }
    };
    let last = a.commit(None);
    eprintln!("T2 next commit: {}", outcome(&last));
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
    assert!(
        disk.first() == Some(&88) || named,
        "the mapped store vanished and nothing named the file"
    );
    let _ = Outcome::UpToDate {
        head: None,
        root: String::new(),
    };
}

/// T3. A committed file is held open by a reader that does not share
/// writing (the .NET default for a read) while this Workspace merges another
/// Workspace's change to it. The directory cannot rewrite the file then.
/// Afterwards the reader is gone: the directory must come to show the
/// Branch's file, or status must say that it does not.
#[test]
fn t3_a_file_the_merge_could_not_rewrite_is_not_left_stale_and_clean() {
    const A1: &[u8] = b"fn main() { a1(); }\n";
    const B2: &[u8] = b"fn main() { b2(); }\n";
    let fixture = Fixture::new("rv3-refused");
    let a = fixture.mount("a");
    let file = a.root().join("src").join("main.rs");
    ops::run(fixture.home.path(), &[Op::Write(file.clone(), A1.to_vec())]);
    a.commit(Some("a1")).expect("a1");
    let b = fixture.mount("b");
    ops::run(
        fixture.home.path(),
        &[Op::Write(b.root().join("src").join("main.rs"), B2.to_vec())],
    );
    b.commit(Some("b2")).expect("b2");
    let (ready, done) = (
        fixture.home.path().join("reader-ready"),
        fixture.home.path().join("reader-done"),
    );
    let mut reader = powershell(&format!(
        "$s=[System.IO.File]::Open('{}','Open','Read','Read'); Set-Content '{}' 'ready'; \
         while (-not (Test-Path '{}')) {{ Start-Sleep -Milliseconds 20 }}; $s.Dispose()",
        file.display(),
        ready.display(),
        done.display()
    ));
    wait_for("the reader holds the file", || ready.exists());
    ops::run(
        fixture.home.path(),
        &[Op::Write(a.root().join("other.txt"), b"other".to_vec())],
    );
    let merged = a.commit(Some("a3"));
    eprintln!("T3 merge commit: {}", outcome(&merged));
    std::fs::write(&done, b"done").expect("signal");
    reader.wait().expect("reader");
    std::thread::sleep(Duration::from_millis(600));
    let status = a.status().expect("status");
    let again = a.commit(None);
    let disk = std::fs::read(&file).expect("read");
    eprintln!(
        "T3 after the reader closed: status {:?}; hidden {:?}; next commit: {}; disk {:?}",
        status.changes,
        status.hidden,
        outcome(&again),
        String::from_utf8_lossy(&disk)
    );
    // The user edits what the directory shows and commits.
    let mut edited = disk.clone();
    edited.extend_from_slice(b"// one more line\n");
    ops::run(fixture.home.path(), &[Op::Write(file.clone(), edited)]);
    let last = a.commit(Some("edit"));
    let branch = fixture.saved("work").1["src/main.rs"];
    let kept = branch == digest(&[B2, b"// one more line\n"].concat());
    eprintln!(
        "T3 after an edit of what the directory showed: {}; the Branch still holds B's change: {kept}",
        outcome(&last)
    );
    let stale_and_clean = disk == A1 && status.changes.is_empty();
    a.discard().expect("discard");
    b.discard().expect("discard");
    fixture.close();
    assert!(
        !stale_and_clean,
        "the directory shows the old file, status is clean, and nothing brings it over"
    );
}
