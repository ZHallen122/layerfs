//! The command line end to end: every step is a separate process, as an
//! agent host would run them. The provider is the foreground `mount`.
#![cfg(windows)]

mod support;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use support::ops::{self, Op};

const BINARY: &str = env!("CARGO_BIN_EXE_layerfs-projfs");

/// Runs one command to its end; its one line of JSON and whether it
/// exited 0.
fn run(arguments: &[&str]) -> (String, bool) {
    let output = Command::new(BINARY).args(arguments).output().expect("run");
    (
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        output.status.success(),
    )
}

/// Starts a serving command and waits for its ready line.
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
fn child_process_entry() {
    ops::child_entry();
}

#[test]
fn init_mount_edit_status_commit_discard() {
    let home = support::Scratch::new("cli");
    let tree = home.path().join("tree");
    support::write_tree(&tree);
    let (store, root) = (home.path().join("store.sqlite"), home.path().join("ws"));
    let store_args = ["--store", text(&store), "--project", "demo"];

    let (created, ok) = run(&[&["init"], &store_args[..], &["--import", text(&tree)]].concat());
    assert!(ok && created.contains("\"entries\":"), "{created}");
    let (before, _) = run(&[&["head"], &store_args[..], &["--branch", "work"]].concat());

    let (mut provider, ready) = serve(
        &[
            &["mount"],
            &store_args[..],
            &["--branch", "work", "--root", text(&root)],
        ]
        .concat(),
    );
    assert!(ready.contains("\"branch\":\"work\""), "{ready}");
    assert_eq!(
        std::fs::read(root.join("README.md")).expect("read"),
        b"# fixture\n"
    );
    let (clean, _) = run(&["status", "--root", text(&root)]);
    assert!(clean.contains("\"changes\":[]"), "{clean}");

    ops::run(
        home.path(),
        &[
            Op::Write(root.join("added.txt"), b"added\n".to_vec()),
            Op::Remove(root.join("empty.bin")),
        ],
    );
    let (status, ok) = run(&["status", "--root", text(&root)]);
    assert!(ok, "{status}");
    assert!(
        status.contains("{\"path\":\"added.txt\",\"kind\":\"added\"")
            && status.contains("{\"path\":\"empty.bin\",\"kind\":\"deleted\""),
        "{status}"
    );
    let (committed, ok) = run(&["commit", "--root", text(&root), "--message", "from the CLI"]);
    assert!(
        ok && committed.contains("\"outcome\":\"Committed\""),
        "{committed}"
    );
    let (stats, _) = run(&["stats", "--root", text(&root)]);
    assert!(stats.contains("\"notifications\":"), "{stats}");

    let (ended, ok) = run(&["discard", "--root", text(&root)]);
    assert!(ok && ended.contains("\"ended\":\"discard\""), "{ended}");
    assert!(provider.wait().expect("provider exit").success());
    assert!(!root.exists(), "the Workspace directory is gone");

    // The Commit outlives the Workspace, and a new mount shows it.
    let (after, _) = run(&[&["head"], &store_args[..], &["--branch", "work"]].concat());
    assert_ne!(after, before, "the Branch head moved");
    let (mut again, _) = serve(
        &[
            &["mount"],
            &store_args[..],
            &["--branch", "work", "--root", text(&root)],
        ]
        .concat(),
    );
    assert_eq!(
        std::fs::read(root.join("added.txt")).expect("read"),
        b"added\n"
    );
    assert!(!root.join("empty.bin").exists());
    let (ended, ok) = run(&["unmount", "--root", text(&root)]);
    assert!(ok, "{ended}");
    assert!(again.wait().expect("provider exit").success());
    // A command to a mount nobody serves is refused, not hung.
    let (refused, ok) = run(&["status", "--root", text(&root)]);
    assert!(!ok && refused.contains("NotMounted"), "{refused}");
}
