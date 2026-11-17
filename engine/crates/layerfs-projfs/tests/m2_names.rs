//! M2: Windows correctness cases about names, metadata and links. One test
//! per case; each asserts what actually happens and records a verdict:
//! PASS, FAIL or UNSUPPORTED with what the user sees.
#![cfg(windows)]

mod support;

use layerfs_projfs::workspace::{CaptureError, CommitRefusal, Outcome};
use layerfs_projfs::{From, Kind};
use layerfs_store_service::read::snapshot;
use std::os::windows::fs::MetadataExt;
use std::process::Command;
use support::fixture::{shape, Fixture};
use support::ops::{self, Op};
use support::{at, tree_digests, verdict};

const READ_ONLY: u32 = 0x1;
const HIDDEN: u32 = 0x2;

#[test]
fn child_process_entry() {
    ops::child_entry();
}

fn committed(result: Result<layerfs_projfs::workspace::Committed, CommitRefusal>) {
    let done = result.expect("commit");
    assert!(
        matches!(done.outcome, Outcome::Committed { .. }),
        "expected a new Commit: {done:?}"
    );
}

#[test]
fn names_differing_only_by_case_merged_from_two_branches() {
    let fixture = Fixture::new("m2-case-merge");
    layerfs_store_service::history::branch::fork(
        fixture.engine.store(),
        &fixture.stack,
        "other",
        Some("work"),
    )
    .expect("fork");
    let (first, second) = (
        fixture.mount("a"),
        fixture.mount_from("b", From::Branch("other")),
    );
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(first.root().join("Config.json"), b"work".to_vec()),
            Op::Write(second.root().join("config.json"), b"other".to_vec()),
        ],
    );
    committed(first.commit(None));
    committed(second.commit(None));
    first.discard().expect("discard");
    second.discard().expect("discard");
    // The existing merge accepts both: the Store's names are case-sensitive.
    layerfs_store_service::history::branch::merge(
        fixture.engine.store(),
        &fixture.stack,
        "work",
        "other",
        "merged",
        Some("merge"),
        &[],
    )
    .expect("merge");
    let (saved, _) = fixture.saved("merged");
    assert!(saved.contains_key("Config.json") && saved.contains_key("config.json"));

    // The mount shows one of them; the other is unreachable and reported.
    let merged = fixture.mount_from("m", From::Branch("merged"));
    let listed: Vec<String> = shape(merged.root())
        .into_keys()
        .filter(|name| name.eq_ignore_ascii_case("config.json"))
        .collect();
    assert_eq!(listed, vec!["Config.json".to_owned()]);
    for spelling in ["Config.json", "config.json"] {
        assert_eq!(
            std::fs::read(merged.root().join(spelling)).expect("read"),
            b"work",
            "{spelling} opens the shown file"
        );
    }
    let status = merged.status().expect("status");
    assert_eq!(status.changes, Vec::new());
    assert_eq!(
        status.hidden,
        vec![(
            "config.json".to_owned(),
            "another name in this directory differs only by case"
        )]
    );
    // An edit and Commit of the shown file leaves the hidden one intact.
    ops::run(
        fixture.home.path(),
        &[Op::Write(
            merged.root().join("Config.json"),
            b"edited".to_vec(),
        )],
    );
    committed(merged.commit(None));
    let (_, digests) = fixture.saved("merged");
    assert_eq!(digests["Config.json"], *blake3::hash(b"edited").as_bytes());
    assert_eq!(digests["config.json"], *blake3::hash(b"other").as_bytes());
    merged.discard().expect("discard");
    fixture.close();
    verdict(
        "Config.json and config.json on two Branches, merged",
        "UNSUPPORTED",
        "The merge succeeds (Store names are case-sensitive). The mount shows only the first \
         name in byte order (Config.json); either spelling opens it. The other file cannot be \
         reached; `status` lists it under `hidden` with the reason, and Commits leave it \
         unchanged in the Store. Nothing warns at merge time.",
    );
}

#[test]
fn a_case_only_rename() {
    let fixture = Fixture::new("m2-case-rename");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[Op::Rename(at(&root, "README.md"), at(&root, "readme.MD"))],
    );
    let changes = workspace.status().expect("status").changes;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].path, "readme.MD");
    assert_eq!(
        changes[0].kind,
        Kind::Renamed {
            from: "README.md".to_owned()
        }
    );
    committed(workspace.commit(None));
    let (saved, _) = fixture.saved("work");
    assert!(saved.contains_key("readme.MD") && !saved.contains_key("README.md"));
    let fresh = fixture.mount("fresh");
    assert!(shape(fresh.root()).contains_key("readme.MD"));
    assert!(!shape(fresh.root()).contains_key("README.md"));
    fresh.discard().expect("discard");
    workspace.discard().expect("discard");
    fixture.close();
    verdict(
        "case-only rename (README.md -> readme.MD)",
        "PASS",
        "status reports one rename; the Commit holds the new spelling only; a fresh mount \
         shows it.",
    );
}

#[test]
fn non_ascii_names() {
    let fixture = Fixture::new("m2-unicode");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    let names = [
        "\u{4e2d}\u{6587}\u{76ee}\u{5f55}/\u{6587}\u{4ef6}.txt",
        "\u{1f600}\u{1f680}/\u{1f4c4}.txt",
        "caf\u{e9}-na\u{ef}ve.txt",
    ];
    let mut edits = Vec::new();
    for name in names {
        edits.push(Op::Mkdir(
            at(&root, name).parent().expect("parent").to_owned(),
        ));
        edits.push(Op::Write(at(&root, name), name.as_bytes().to_vec()));
    }
    ops::run(fixture.home.path(), &edits);
    committed(workspace.commit(None));
    let (saved, digests) = fixture.saved("work");
    for name in names {
        assert!(saved.contains_key(name), "{name} is in the Commit");
        assert_eq!(digests[name], *blake3::hash(name.as_bytes()).as_bytes());
    }
    let fresh = fixture.mount("fresh");
    assert_eq!(shape(fresh.root()), saved);
    assert_eq!(tree_digests(fresh.root()), digests);
    fresh.discard().expect("discard");
    workspace.discard().expect("discard");
    fixture.close();
    verdict(
        "non-ASCII names (Chinese, emoji, accents)",
        "PASS",
        "created, committed, read back from the Store and shown by a fresh mount with the \
         same names and bytes.",
    );
}

#[test]
fn a_name_over_255_bytes_of_utf8() {
    let fixture = Fixture::new("m2-long-name");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    // 100 Chinese characters: 100 UTF-16 units (legal on NTFS), 300 bytes
    // of UTF-8 (over the Store's 255-byte name limit).
    let name = "\u{957f}".repeat(100) + ".txt";
    assert!(name.len() > 255 && name.encode_utf16().count() < 255);
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(root.join(&name), b"long".to_vec()),
            Op::Write(root.join("ordinary.txt"), b"ordinary".to_vec()),
        ],
    );
    let head = fixture.head();
    let refused = workspace.commit(None).expect_err("the Commit is refused");
    let CommitRefusal::Capture(CaptureError::Unrepresentable(paths)) = &refused else {
        panic!("expected an unrepresentable name: {refused:?}");
    };
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].0, name);
    assert_eq!(
        refused.to_string(),
        "Unsupported: 1 name(s) the Store cannot hold"
    );
    assert_eq!(fixture.head(), head, "nothing was published");
    // Renaming the file to a name that fits lets the same changes commit.
    ops::run(
        fixture.home.path(),
        &[Op::Rename(root.join(&name), root.join("short.txt"))],
    );
    committed(workspace.commit(None));
    let (saved, _) = fixture.saved("work");
    assert!(saved.contains_key("short.txt") && saved.contains_key("ordinary.txt"));
    workspace.discard().expect("discard");
    fixture.close();
    verdict(
        "a name whose UTF-8 form is over 255 bytes",
        "UNSUPPORTED",
        "NTFS accepts the name (it counts UTF-16 units); the Store's name limit is 255 bytes. \
         The whole Commit is refused with `Unsupported: 1 name(s) the Store cannot hold` and \
         names the path; nothing is published. After the file is renamed or removed the \
         Commit succeeds.",
    );
}

#[test]
fn a_path_over_260_characters() {
    let fixture = Fixture::new("m2-long-path");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    let relative = format!(
        "{}/{}/{}/leaf.txt",
        "d".repeat(100),
        "e".repeat(100),
        "f".repeat(100)
    );
    let deep = at(&root, &relative);
    assert!(deep.to_string_lossy().len() > 260);
    ops::run(
        fixture.home.path(),
        &[
            Op::Mkdir(deep.parent().expect("parent").to_owned()),
            Op::Write(deep.clone(), b"deep".to_vec()),
        ],
    );
    committed(workspace.commit(None));
    let (saved, digests) = fixture.saved("work");
    assert_eq!(digests[&relative], *blake3::hash(b"deep").as_bytes());
    // A fresh mount projects it, and it can be edited and committed there.
    let fresh = fixture.mount("fresh");
    assert_eq!(shape(fresh.root()), saved);
    let projected = at(fresh.root(), &relative);
    assert_eq!(std::fs::read(&projected).expect("read"), b"deep");
    ops::run(
        fixture.home.path(),
        &[Op::Write(projected, b"deeper".to_vec())],
    );
    committed(fresh.commit(None));
    let (_, digests) = fixture.saved("work");
    assert_eq!(digests[&relative], *blake3::hash(b"deeper").as_bytes());
    fresh.discard().expect("discard");
    workspace.discard().expect("discard");
    fixture.close();
    verdict(
        "paths over 260 characters",
        "PASS",
        "a 300+ character relative path is created, committed, projected by a fresh mount, \
         edited there and committed again.",
    );
}

#[test]
fn the_read_only_attribute_survives() {
    let fixture = Fixture::new("m2-read-only");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[
            // a new read-only file
            Op::Write(root.join("new-locked.txt"), b"new".to_vec()),
            Op::ReadOnly(root.join("new-locked.txt"), true),
            // an existing file made read-only, content untouched
            Op::ReadOnly(root.join("README.md"), true),
            // an existing read-only file edited and locked again
            Op::ReadOnly(at(&root, "locked/read-only.txt"), false),
            Op::Write(at(&root, "locked/read-only.txt"), b"edited".to_vec()),
            Op::ReadOnly(at(&root, "locked/read-only.txt"), true),
            // an existing file whose read-only attribute is removed
            Op::Write(root.join("was-locked.txt"), b"x".to_vec()),
        ],
    );
    let changes = workspace.status().expect("status").changes;
    assert!(
        changes
            .iter()
            .any(|c| c.path == "README.md" && c.kind == Kind::Modified),
        "an attribute-only change is a change: {changes:?}"
    );
    committed(workspace.commit(None));
    assert_eq!(workspace.status().expect("status").changes, Vec::new());
    let fresh = fixture.mount("fresh");
    let read_only = |name: &str| {
        std::fs::metadata(at(fresh.root(), name))
            .expect("metadata")
            .file_attributes()
            & READ_ONLY
            != 0
    };
    assert!(read_only("new-locked.txt"));
    assert!(read_only("README.md"));
    assert!(read_only("locked/read-only.txt"));
    assert!(!read_only("was-locked.txt"));
    assert_eq!(
        std::fs::read(at(fresh.root(), "locked/read-only.txt")).expect("read"),
        b"edited"
    );
    // Clearing it on the fresh mount survives the next Commit too.
    ops::run(
        fixture.home.path(),
        &[Op::ReadOnly(fresh.root().join("README.md"), false)],
    );
    committed(fresh.commit(None));
    fresh.discard().expect("discard");
    let again = fixture.mount("again");
    assert_eq!(
        std::fs::metadata(again.root().join("README.md"))
            .expect("metadata")
            .file_attributes()
            & READ_ONLY,
        0
    );
    again.discard().expect("discard");
    workspace.discard().expect("discard");
    fixture.close();
    verdict(
        "read-only attribute across mount, edit, Commit, remount",
        "PASS",
        "stored as the file mode's write bits (0o444 / 0o644). ProjFS sends no notification \
         for an attribute change, so status and Commit list the directories on disk to find \
         one; cost follows the files on disk.",
    );
}

#[test]
fn the_hidden_attribute_is_lost() {
    let fixture = Fixture::new("m2-hidden");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[Op::Write(root.join("secret.txt"), b"hidden".to_vec())],
    );
    for name in ["secret.txt", "README.md"] {
        let done = Command::new("attrib")
            .arg("+h")
            .arg(root.join(name))
            .status()
            .expect("attrib");
        assert!(done.success());
    }
    let hidden = |root: &std::path::Path, name: &str| {
        std::fs::metadata(root.join(name))
            .expect("metadata")
            .file_attributes()
            & HIDDEN
            != 0
    };
    assert!(hidden(&root, "secret.txt") && hidden(&root, "README.md"));
    committed(workspace.commit(None));
    // It stays on the local files of this Workspace, and nowhere else.
    assert!(hidden(&root, "secret.txt"));
    let fresh = fixture.mount("fresh");
    assert_eq!(
        std::fs::read(fresh.root().join("secret.txt")).expect("read"),
        b"hidden"
    );
    assert!(!hidden(fresh.root(), "secret.txt"));
    assert!(!hidden(fresh.root(), "README.md"));
    fresh.discard().expect("discard");
    workspace.discard().expect("discard");
    fixture.close();
    verdict(
        "hidden attribute across mount, edit, Commit, remount",
        "UNSUPPORTED",
        "LayerFS portable metadata is mode and mtime. The file's bytes commit; the hidden \
         attribute stays on this Workspace's local file only and is absent after a remount. \
         Nothing warns. System, archive, ACLs and alternate data streams are likewise not \
         stored.",
    );
}

#[test]
fn delete_then_recreate_the_same_name() {
    let fixture = Fixture::new("m2-recreate");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[
            Op::Remove(root.join("README.md")),
            Op::Write(root.join("README.md"), b"recreated".to_vec()),
            Op::RemoveTree(root.join("docs")),
            Op::Mkdir(root.join("docs")),
            Op::Write(at(&root, "docs/again.txt"), b"again".to_vec()),
            // a file replaced by a directory of the same name
            Op::Remove(root.join("empty.bin")),
            Op::Mkdir(root.join("empty.bin")),
            Op::Write(at(&root, "empty.bin/inside.txt"), b"inside".to_vec()),
        ],
    );
    let changes = workspace.status().expect("status").changes;
    assert!(changes
        .iter()
        .any(|c| c.path == "README.md" && c.kind == Kind::Modified));
    assert!(changes
        .iter()
        .any(|c| c.path == "docs/again.txt" && c.kind == Kind::Added));
    committed(workspace.commit(None));
    let (saved, digests) = fixture.saved("work");
    assert_eq!(digests["README.md"], *blake3::hash(b"recreated").as_bytes());
    let docs: Vec<&String> = saved.keys().filter(|k| k.starts_with("docs")).collect();
    assert_eq!(docs, ["docs", "docs/again.txt"]);
    assert_eq!(saved["empty.bin"], (true, 0), "now a directory");
    assert!(saved.contains_key("empty.bin/inside.txt"));
    let fresh = fixture.mount("fresh");
    assert_eq!(shape(fresh.root()), saved);
    assert_eq!(tree_digests(fresh.root()), digests);
    fresh.discard().expect("discard");
    workspace.discard().expect("discard");
    fixture.close();
    verdict(
        "delete then recreate the same name before Commit",
        "PASS",
        "a recreated file commits as modified; a recreated directory holds only its new \
         content; a file replaced by a directory commits as a directory.",
    );
}

#[test]
fn an_editor_style_save() {
    let fixture = Fixture::new("m2-editor-save");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    let saved_bytes = b"fn main() { saved(); }\n";
    ops::run(
        fixture.home.path(),
        &[
            Op::Write(at(&root, "src/main.rs.tmp~"), saved_bytes.to_vec()),
            Op::Rename(at(&root, "src/main.rs.tmp~"), at(&root, "src/main.rs")),
        ],
    );
    let changes = workspace.status().expect("status").changes;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].path, "src/main.rs");
    assert_eq!(changes[0].kind, Kind::Modified);
    let before = snapshot::open(fixture.engine.store(), &fixture.stack, "work")
        .and_then(|(reader, snap)| snapshot::find(reader.as_ref(), &snap.tree, "/src/main.rs"))
        .expect("lookup")
        .expect("entry")
        .serial;
    committed(workspace.commit(None));
    let (saved, digests) = fixture.saved("work");
    assert_eq!(
        digests["src/main.rs"],
        *blake3::hash(saved_bytes).as_bytes()
    );
    assert!(!saved.contains_key("src/main.rs.tmp~"));
    let after = snapshot::open(fixture.engine.store(), &fixture.stack, "work")
        .and_then(|(reader, snap)| snapshot::find(reader.as_ref(), &snap.tree, "/src/main.rs"))
        .expect("lookup")
        .expect("entry")
        .serial;
    assert_eq!(after, before, "the file keeps its inode in the Store");
    workspace.discard().expect("discard");
    fixture.close();
    verdict(
        "editor-style save (write a temporary file, rename over the original)",
        "PASS",
        "status reports the original as modified and nothing else; the Commit holds the new \
         bytes under the original name with its inode kept, and no temporary file.",
    );
}

#[test]
fn hard_links() {
    let fixture = Fixture::new("m2-hard-links");
    let workspace = fixture.mount("ws");
    let root = workspace.root().to_owned();
    ops::run(
        fixture.home.path(),
        &[
            // a link to a projected file, and one between two new files
            Op::HardLink(root.join("README.md"), root.join("readme-link.md")),
            Op::Write(root.join("orig.txt"), b"linked".to_vec()),
            Op::HardLink(root.join("orig.txt"), root.join("link.txt")),
        ],
    );
    committed(workspace.commit(None));
    let serial = |name: &str| {
        snapshot::open(fixture.engine.store(), &fixture.stack, "work")
            .and_then(|(reader, snap)| snapshot::find(reader.as_ref(), &snap.tree, name))
            .expect("lookup")
            .expect("entry")
    };
    // In the Store both pairs are one inode with two names.
    assert_eq!(
        serial("/README.md").serial,
        serial("/readme-link.md").serial
    );
    assert_eq!(serial("/orig.txt").serial, serial("/link.txt").serial);
    assert_eq!(serial("/orig.txt").value.namespace_ref_count, 2);
    workspace.discard().expect("discard");

    // A later mount projects each name as its own file.
    let fresh = fixture.mount("fresh");
    let links = |name: &str| {
        layerfs_projfs::sys::identity(&std::fs::File::open(fresh.root().join(name)).expect("open"))
            .expect("identity")
    };
    let (left, right) = (links("orig.txt"), links("link.txt"));
    assert_ne!(left.index, right.index, "two files on disk");
    assert_eq!((left.links, right.links), (1, 1));
    // Writing through one name changes only that name in the directory...
    ops::run(
        fixture.home.path(),
        &[Op::Write(
            fresh.root().join("orig.txt"),
            b"changed".to_vec(),
        )],
    );
    assert_eq!(
        std::fs::read(fresh.root().join("link.txt")).expect("read"),
        b"linked"
    );
    committed(fresh.commit(None));
    // ...but the Store's one inode has both names, so both change there.
    let (_, digests) = fixture.saved("work");
    assert_eq!(digests["orig.txt"], *blake3::hash(b"changed").as_bytes());
    assert_eq!(digests["link.txt"], *blake3::hash(b"changed").as_bytes());
    assert_eq!(serial("/orig.txt").serial, serial("/link.txt").serial);
    let stale = std::fs::read(fresh.root().join("link.txt")).expect("read");
    let status = fresh.status().expect("status").changes;
    fresh.discard().expect("discard");
    fixture.close();
    assert_eq!(stale, b"linked", "this mount's other name is stale");
    assert_eq!(status, Vec::new(), "and status does not report it");
    verdict(
        "hard links",
        "UNSUPPORTED",
        "Creating a link works, to a projected file too, and its Commit stores one inode with \
         two names. ProjFS cannot project one file under two names: a later mount shows two \
         independent files. A write through one name then commits to the shared inode, so the \
         Store changes both names while this mount's other name keeps the old bytes until it \
         is remounted; status does not report it.",
    );
}
