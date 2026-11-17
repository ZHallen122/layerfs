//! M2: Windows correctness cases that involve other processes: open files,
//! memory-mapped writes, a killed provider, Git and a file watcher. One test
//! per case; each asserts what actually happens and records a verdict.
#![cfg(windows)]

mod support;

use layerfs_projfs::workspace::Outcome;
use layerfs_projfs::{From, Kind, MountRequest, Workspace};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use support::fixture::Fixture;
use support::ops::{self, Op};
use support::{at, verdict};

const BINARY: &str = env!("CARGO_BIN_EXE_layerfs-projfs");
/// "The provider that supports file system virtualization is temporarily
/// unavailable."
const PROVIDER_UNAVAILABLE: i32 = 369;

#[test]
fn child_process_entry() {
    ops::child_entry();
}

fn powershell(script: &str) -> Child {
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdout(Stdio::null())
        .spawn()
        .expect("powershell")
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

#[test]
fn a_file_held_open_by_another_process_during_commit() {
    let fixture = Fixture::new("m2-held-open");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    // One process keeps a new file open for writing, another an existing one.
    let (mut new_file, _) = ops::spawn(
        fixture.home.path(),
        &[Op::Hold(
            root.join("held-new.txt"),
            b"written while held".to_vec(),
            2500,
        )],
    );
    let (mut existing, _) = ops::spawn(
        fixture.home.path(),
        &[Op::Hold(root.join("README.md"), b"HELD".to_vec(), 2500)],
    );
    wait_for("both writers wrote", || {
        std::fs::read(root.join("held-new.txt")).is_ok_and(|bytes| !bytes.is_empty())
            && std::fs::read(root.join("README.md")).is_ok_and(|bytes| bytes.starts_with(b"HELD"))
    });
    let committed = workspace.commit(None).expect("commit while held");
    assert!(matches!(committed.outcome, Outcome::Committed { .. }));
    let mut live = committed.captured.live.clone();
    live.sort();
    assert_eq!(live, ["README.md", "held-new.txt"]);
    assert!(new_file.try_wait().expect("child").is_none(), "still open");
    let (_, digests) = fixture.saved("work");
    assert_eq!(
        digests["held-new.txt"],
        *blake3::hash(b"written while held").as_bytes()
    );
    assert_eq!(
        digests["README.md"],
        *blake3::hash(b"HELDxture\n").as_bytes()
    );
    // They stay journaled: the Commit after they close looks at them again.
    new_file.wait().expect("child");
    existing.wait().expect("child");
    let again = workspace.commit(None).expect("commit after close");
    assert!(
        matches!(again.outcome, Outcome::UpToDate { .. }),
        "{again:?}"
    );
    assert!(
        again.captured.bytes_read > 0,
        "the held files were read again"
    );
    assert_eq!(again.captured.live, Vec::<String>::new());
    workspace.discard().expect("discard");
    fixture.close();
    verdict(
        "a file held open by another process during Commit",
        "PASS",
        "The Commit succeeds with the bytes on disk at that moment and names the file in \
         `live`. A file open for writing cannot be opened deny-write, so it is read shared \
         and its size and time are compared before and after; a change during the read \
         refuses the Commit as `Busy`. Such a file stays journaled and is compared again by \
         the next Commit.",
    );
}

#[test]
fn a_file_written_through_a_memory_mapped_view() {
    let fixture = Fixture::new("m2-mmap");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    let committed_bytes = |name: &str| fixture.saved("work").1[name];

    // 1. A mapping whose file handle shares reading and writing. Two bytes
    //    are stored through the view, which stays mapped and unflushed.
    let shared = at(&root, "src/lib/deep/mod.rs");
    let marker = fixture.home.path().join("mapped-shared");
    let mut mapper = powershell(&format!(
        "$s=[System.IO.File]::Open('{}','Open','ReadWrite','ReadWrite'); \
         $f=[System.IO.MemoryMappedFiles.MemoryMappedFile]::CreateFromFile($s,[NullString]::Value,0,\
         [System.IO.MemoryMappedFiles.MemoryMappedFileAccess]::ReadWrite,$null,\
         [System.IO.HandleInheritability]::None,$false); $a=$f.CreateViewAccessor(); \
         $a.Write(0,[byte]88); $a.Write(1,[byte]89); Set-Content '{}' 'mapped'; \
         Start-Sleep -Milliseconds 2500; $a.Dispose(); $f.Dispose(); $s.Dispose()",
        shared.display(),
        marker.display()
    ));
    wait_for("the shared view is written", || marker.exists());
    let mut expected = support::pattern(1, 4097);
    expected[..2].copy_from_slice(b"XY");
    let changes = workspace.status().expect("status").changes;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].path, "src/lib/deep/mod.rs");
    assert_eq!(changes[0].kind, Kind::Modified);
    let during = workspace.commit(None).expect("commit while mapped");
    assert!(matches!(during.outcome, Outcome::Committed { .. }));
    assert_eq!(during.captured.live, ["src/lib/deep/mod.rs"]);
    assert_eq!(
        committed_bytes("src/lib/deep/mod.rs"),
        *blake3::hash(&expected).as_bytes(),
        "the Commit holds the bytes stored through the still-mapped view"
    );
    assert!(mapper.wait().expect("powershell").success());

    // 2. The .NET default: the mapping's file is opened exclusively, so no
    //    other process can read it while it is mapped.
    let exclusive = root.join("README.md");
    let marker = fixture.home.path().join("mapped-exclusive");
    let mut mapper = powershell(&format!(
        "$f=[System.IO.MemoryMappedFiles.MemoryMappedFile]::CreateFromFile('{}',\
         [System.IO.FileMode]::Open); $a=$f.CreateViewAccessor(); $a.Write(0,[byte]90); \
         Set-Content '{}' 'mapped'; Start-Sleep -Milliseconds 2500; $a.Dispose(); $f.Dispose()",
        exclusive.display(),
        marker.display()
    ));
    wait_for("the exclusive view is written", || marker.exists());
    let changes = workspace.status().expect("status").changes;
    assert!(
        changes
            .iter()
            .any(|c| c.path == "README.md" && c.kind == Kind::Modified),
        "an unreadable full file is reported as modified: {changes:?}"
    );
    let head = fixture.head();
    let refused = workspace.commit(None).expect_err("the file cannot be read");
    assert_eq!(
        refused.to_string(),
        "Busy: README.md is held open exclusively by another process"
    );
    assert_eq!(fixture.head(), head, "nothing was published");
    assert!(mapper.wait().expect("powershell").success());
    let after = workspace.commit(None).expect("commit after unmap");
    assert!(
        matches!(after.outcome, Outcome::Committed { .. }),
        "{after:?}"
    );
    assert_eq!(
        committed_bytes("README.md"),
        *blake3::hash(b"Z fixture\n").as_bytes()
    );
    workspace.discard().expect("discard");
    fixture.close();
    verdict(
        "a file written through a memory-mapped view",
        "PASS",
        "Opening a file for a writable mapping converts it to a full file and journals it. \
         With a shared file handle, a Commit while the view is still mapped and unflushed \
         stores the bytes written through it (Windows serves reads and views from one cache) \
         and lists the file in `live`. With an exclusive handle (the .NET default) nobody can \
         read the file: status reports it modified and the Commit is refused `Busy`, naming \
         it, with nothing published; it commits once unmapped. Not covered: a store through \
         a view kept after its file handle closed raises no notification.",
    );
}

fn run(arguments: &[&str]) -> String {
    let output = Command::new(BINARY).args(arguments).output().expect("run");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn serve(arguments: &[&str]) -> (Child, String) {
    let mut child = Command::new(BINARY)
        .args(arguments)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("ready line");
    assert!(line.contains("\"mounted\":true"), "not mounted: {line}");
    (child, line)
}

fn text(path: &Path) -> &str {
    path.to_str().expect("UTF-8 path")
}

#[test]
fn the_provider_is_killed_and_restarted() {
    let home = support::Scratch::new("m2-killed");
    let tree = home.path().join("tree");
    support::write_tree(&tree);
    let (store, root) = (home.path().join("store.sqlite"), home.path().join("ws"));
    let store_args = ["--store", text(&store), "--project", "demo"];
    run(&[&["init"], &store_args[..], &["--import", text(&tree)]].concat());
    let (mut provider, _) = serve(
        &[
            &["mount"],
            &store_args[..],
            &["--branch", "work", "--root", text(&root)],
        ]
        .concat(),
    );
    // README.md is read (hydrated); src/main.rs is only looked at (a
    // placeholder); docs and locked are never touched.
    assert_eq!(
        std::fs::read(root.join("README.md")).expect("read"),
        b"# fixture\n"
    );
    std::fs::metadata(at(&root, "src/main.rs")).expect("metadata");
    ops::run(
        home.path(),
        &[
            Op::Write(root.join("before-kill.txt"), b"before".to_vec()),
            Op::Remove(root.join("empty.bin")),
            Op::Write(at(&root, "assets/large.bin"), b"shrunk".to_vec()),
        ],
    );
    provider.kill().expect("kill");
    provider.wait().expect("wait");

    // What is visible with no provider: only what is on the local disk.
    let listed = |directory: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    };
    // The deleted file's tombstone shows as a name; the directories nobody
    // opened (docs, locked) are not there at all.
    assert_eq!(
        listed(&root),
        ["README.md", "assets", "before-kill.txt", "empty.bin", "src"]
    );
    assert_eq!(
        std::fs::read(root.join("README.md")).expect("hydrated"),
        b"# fixture\n"
    );
    assert_eq!(
        std::fs::read(root.join("before-kill.txt")).expect("full"),
        b"before"
    );
    assert_eq!(
        std::fs::read(at(&root, "assets/large.bin")).expect("full"),
        b"shrunk"
    );
    let unavailable = std::fs::read(at(&root, "src/main.rs")).expect_err("a placeholder's data");
    assert!(
        matches!(unavailable.raw_os_error(), Some(369 | 372)),
        "{unavailable:?}"
    );
    assert_eq!(
        std::fs::read_dir(root.join("docs"))
            .expect_err("never opened")
            .raw_os_error(),
        Some(PROVIDER_UNAVAILABLE)
    );
    // Changes while it is down: creating and rewriting are refused; a full
    // file can be deleted.
    let down = ops::attempt(
        home.path(),
        &[
            Op::Write(root.join("while-down.txt"), b"down".to_vec()),
            Op::Write(root.join("README.md"), b"rewritten".to_vec()),
            Op::Remove(root.join("before-kill.txt")),
        ],
    );
    assert_eq!(down[0].clone().map_err(|e| e.0), Err(PROVIDER_UNAVAILABLE));
    assert_eq!(down[1].clone().map_err(|e| e.0), Err(PROVIDER_UNAVAILABLE));
    assert_eq!(down[2], Ok(()));
    assert!(run(&["status", "--root", text(&root)]).contains("NotMounted"));

    // Restart: the directory is served again with nothing lost.
    let (mut resumed, ready) = serve(&["resume", "--store", text(&store), "--root", text(&root)]);
    assert!(ready.contains("\"scanned_changes\":2"), "{ready}");
    let status = run(&["status", "--root", text(&root)]);
    assert!(
        status.contains("{\"path\":\"assets/large.bin\",\"kind\":\"modified\"")
            && status.contains("{\"path\":\"empty.bin\",\"kind\":\"deleted\"")
            && !status.contains("before-kill.txt"),
        "{status}"
    );
    assert_eq!(
        std::fs::read(at(&root, "src/main.rs"))
            .expect("served again")
            .len(),
        13
    );
    assert!(root.join("docs").is_dir());
    let committed = run(&["commit", "--root", text(&root)]);
    assert!(
        committed.contains("\"outcome\":\"Committed\""),
        "{committed}"
    );
    assert!(run(&["discard", "--root", text(&root)]).contains("\"ended\""));
    assert!(resumed.wait().expect("exit").success());
    verdict(
        "provider process killed mid-session, then restarted",
        "PASS",
        "While no provider runs: files already read or written stay readable; a file never \
         read fails with error 369 or 372 and a directory never opened with 369; creating a \
         file or rewriting a hydrated one fails with 369; deleting a full file works; a \
         deleted file's tombstone shows as a name. `resume` scans the local disk before \
         serving, so changes from before the kill and from the downtime are both in status. \
         Nothing written is lost. The Branch is bound at its head as of the restart.",
    );
}

fn git(directory: &Path, arguments: &[&str]) -> (bool, String) {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "core.autocrlf=false",
        ])
        .args(arguments)
        .output()
        .expect("git");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
    )
}

/// A Store whose Branch `work` holds a Git repository with two commits.
fn git_fixture(
    label: &str,
) -> (
    support::Scratch,
    std::sync::Arc<layerfs_projfs::store::Engine>,
) {
    let home = support::Scratch::new(label);
    let tree = home.path().join("tree");
    support::write_tree(&tree);
    assert!(git(&tree, &["init", "-q", "-b", "trunk"]).0);
    assert!(git(&tree, &["add", "-A"]).0);
    assert!(git(&tree, &["commit", "-q", "-m", "first"]).0);
    std::fs::write(tree.join("second.txt"), b"second\n").expect("write");
    assert!(git(&tree, &["add", "-A"]).0);
    assert!(git(&tree, &["commit", "-q", "-m", "second"]).0);
    let store = home.path().join("store.sqlite");
    layerfs_projfs::store::init_project(&store, "demo", "work", &tree).expect("Project Init");
    let engine = std::sync::Arc::new(
        layerfs_projfs::store::Engine::open(&store, &home.path().join("overlay")).expect("engine"),
    );
    (home, engine)
}

fn mount(engine: &std::sync::Arc<layerfs_projfs::store::Engine>, root: &Path) -> Workspace {
    Workspace::mount(
        engine,
        MountRequest {
            project: "demo",
            from: From::Branch("work"),
            root,
            state: None,
            threads: 0,
        },
    )
    .expect("mount")
}

#[test]
fn git_status_and_checkout_inside_the_workspace() {
    let (home, engine) = git_fixture("m2-git");
    let root = home.path().join("ws");
    let workspace = mount(&engine, &root);
    // A freshly mounted repository is clean, and asking is not a change.
    assert_eq!(
        git(&root, &["status", "--porcelain"]),
        (true, String::new())
    );
    assert_eq!(workspace.status().expect("status").changes, Vec::new());
    ops::run(
        home.path(),
        &[Op::Write(
            at(&root, "src/main.rs"),
            b"fn main() { changed(); }\n".to_vec(),
        )],
    );
    assert_eq!(
        git(&root, &["status", "--porcelain"]),
        (true, "M src/main.rs".to_owned())
    );
    assert!(git(&root, &["checkout", "-q", "-b", "feature"]).0);
    assert!(git(&root, &["commit", "-q", "-am", "feature work"]).0);
    // Check out the first commit: Git removes the file the second added.
    assert!(git(&root, &["checkout", "-q", "trunk~1"]).0);
    assert!(!root.join("second.txt").exists());
    assert_eq!(
        std::fs::read(at(&root, "src/main.rs")).expect("read"),
        b"fn main() {}\n"
    );
    assert!(git(&root, &["checkout", "-q", "feature"]).0);
    assert!(root.join("second.txt").exists());
    assert_eq!(
        git(&root, &["status", "--porcelain"]),
        (true, String::new())
    );
    let log = git(&root, &["log", "--format=%s"]).1;
    assert_eq!(log, "feature work\nsecond\nfirst");

    // The LayerFS Commit carries the whole repository state, .git included.
    let changes = workspace.status().expect("status").changes;
    assert!(changes.iter().any(|c| c.path == ".git/HEAD"), "{changes:?}");
    assert!(changes.iter().any(|c| c.path == "src/main.rs"));
    assert!(matches!(
        workspace.commit(None).expect("commit").outcome,
        Outcome::Committed { .. }
    ));
    let fresh_root = home.path().join("fresh");
    let fresh = mount(&engine, &fresh_root);
    assert_eq!(
        git(&fresh_root, &["status", "--porcelain"]),
        (true, String::new())
    );
    assert_eq!(git(&fresh_root, &["log", "--format=%s"]).1, log);
    assert_eq!(
        git(&fresh_root, &["rev-parse", "--abbrev-ref", "HEAD"]).1,
        "feature"
    );
    assert!(git(&fresh_root, &["fsck", "--no-progress"]).0);
    fresh.discard().expect("discard");
    workspace.discard().expect("discard");
    std::sync::Arc::try_unwrap(engine)
        .ok()
        .expect("engine")
        .close()
        .expect("close");
    verdict(
        "git status and git checkout inside the Workspace",
        "PASS",
        "git status is clean on a fresh mount and after edits reports them; checkout -b, \
         commit, checkout of an older commit (files removed and rewritten) and back all \
         succeed. The LayerFS Commit includes .git; a fresh mount of it has a clean status, \
         the same log and branch, and passes git fsck.",
    );
}

#[test]
fn a_file_watcher_inside_the_workspace() {
    let (home, engine) = git_fixture("m2-watcher");
    let root = home.path().join("ws");
    let workspace = mount(&engine, &root);
    let (events, ready) = (home.path().join("events.txt"), home.path().join("ready"));
    let script = format!(
        "$w = New-Object System.IO.FileSystemWatcher '{root}'; \
         $w.IncludeSubdirectories = $true; $w.EnableRaisingEvents = $true; \
         $q = New-Object System.Collections.ArrayList; \
         foreach ($n in 'Created','Changed','Deleted','Renamed') \
         {{ Register-ObjectEvent $w $n -SourceIdentifier $n | Out-Null }}; \
         Set-Content '{ready}' 'ready'; $end = (Get-Date).AddSeconds(4); \
         while ((Get-Date) -lt $end) {{ $e = Wait-Event -Timeout 1; if ($e) \
         {{ [void]$q.Add($e.SourceIdentifier + ' ' + $e.SourceEventArgs.Name); \
         Remove-Event -EventIdentifier $e.EventIdentifier }} }}; \
         Set-Content '{events}' ($q -join \"`n\")",
        root = root.display(),
        ready = ready.display(),
        events = events.display()
    );
    let mut watcher = powershell(&script);
    wait_for("the watcher is registered", || ready.exists());
    ops::run(
        home.path(),
        &[
            Op::Write(root.join("watched-new.txt"), b"new".to_vec()),
            Op::Write(root.join("README.md"), b"edited".to_vec()),
            Op::Rename(root.join("second.txt"), root.join("second-renamed.txt")),
            Op::Remove(root.join("empty.bin")),
        ],
    );
    assert!(watcher.wait().expect("watcher").success());
    let seen = std::fs::read_to_string(&events).expect("events");
    let seen: Vec<&str> = seen.lines().map(str::trim).collect();
    for expected in [
        "Created watched-new.txt",
        "Changed README.md",
        "Renamed second-renamed.txt",
        "Deleted empty.bin",
    ] {
        assert!(seen.contains(&expected), "missing `{expected}` in {seen:?}");
    }
    // The ProjFS difference: hiding a projected name writes a tombstone,
    // which a watcher sees as the old name being created.
    for extra in ["Created second.txt", "Created empty.bin"] {
        assert!(seen.contains(&extra), "missing `{extra}` in {seen:?}");
    }
    assert!(!root.join("empty.bin").exists() && !root.join("second.txt").exists());
    workspace.discard().expect("discard");
    std::sync::Arc::try_unwrap(engine)
        .ok()
        .expect("engine")
        .close()
        .expect("close");
    verdict(
        "a file watcher (FileSystemWatcher) inside the Workspace",
        "PASS",
        "Created, Changed, Renamed and Deleted events arrive for changes made by another \
         process. Difference from plain NTFS: deleting or renaming a projected entry is \
         followed by an extra `Created <old name>` event (ProjFS writes a tombstone), although \
         the name no longer exists.",
    );
}
