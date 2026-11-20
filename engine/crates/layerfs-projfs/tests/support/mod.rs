//! Shared test helpers: scratch directories, tree fixtures and hashing.
#![allow(dead_code)]

pub mod fixture;
pub mod ops;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// Where test scratch lives. ProjFS needs a local NTFS volume and the Store
/// a local filesystem; `LAYERFS_PROJFS_TEST_DIR` overrides the temp directory.
pub fn scratch_root() -> PathBuf {
    std::env::var_os("LAYERFS_PROJFS_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// A unique directory removed when dropped, unless the test panicked: a
/// failed test keeps its state for diagnosis.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let path = scratch_root().join(format!(
            "layerfs-projfs-{label}-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self(path)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("kept for diagnosis: {}", self.0.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Deterministic bytes: a cheap generator, different for every seed.
pub fn pattern(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// Writes a small fixed tree below `root` and returns its files as
/// (`/`-separated path, bytes): nested directories, an empty file, a file
/// over one megabyte, a read-only file, and Chinese and emoji names.
pub fn write_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let files: Vec<(String, Vec<u8>)> = vec![
        ("README.md".to_owned(), b"# fixture\n".to_vec()),
        ("empty.bin".to_owned(), Vec::new()),
        ("src/main.rs".to_owned(), b"fn main() {}\n".to_vec()),
        ("src/lib/deep/mod.rs".to_owned(), pattern(1, 4097)),
        ("assets/large.bin".to_owned(), pattern(2, (1 << 20) + 123)),
        (
            "docs/\u{4e2d}\u{6587}\u{6587}\u{6863}.txt".to_owned(),
            pattern(3, 300),
        ),
        (
            "docs/\u{1f680}-launch.txt".to_owned(),
            b"emoji name\n".to_vec(),
        ),
        ("locked/read-only.txt".to_owned(), b"read only\n".to_vec()),
    ];
    for (path, bytes) in &files {
        let native = path
            .split('/')
            .fold(root.to_owned(), |at, name| at.join(name));
        std::fs::create_dir_all(native.parent().expect("parent")).expect("directories");
        std::fs::write(&native, bytes).expect("fixture file");
    }
    let locked = root.join("locked").join("read-only.txt");
    let mut permissions = std::fs::metadata(&locked).expect("metadata").permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&locked, permissions).expect("read-only");
    files
}

/// The BLAKE3 digest of a file, read in bounded windows.
pub fn file_digest(path: &Path) -> [u8; 32] {
    use std::io::Read;
    let mut file = std::fs::File::open(path).expect("open for digest");
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).expect("read for digest");
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    *hasher.finalize().as_bytes()
}

/// Every entry below `root`: `/`-separated path to (is directory, size,
/// last write ticks). A directory's size and time are reported as 0.
pub fn tree_state(root: &Path) -> std::collections::BTreeMap<String, (bool, u64, u64)> {
    use std::os::windows::fs::MetadataExt;
    fn walk(
        at: &Path,
        prefix: &str,
        into: &mut std::collections::BTreeMap<String, (bool, u64, u64)>,
    ) {
        for entry in std::fs::read_dir(at).expect("read_dir") {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let metadata = entry.metadata().expect("metadata");
            if metadata.is_dir() {
                into.insert(path.clone(), (true, 0, 0));
                walk(&entry.path(), &path, into);
            } else {
                into.insert(
                    path,
                    (false, metadata.file_size(), metadata.last_write_time()),
                );
            }
        }
    }
    let mut state = std::collections::BTreeMap::new();
    walk(root, "", &mut state);
    state
}

/// The digest of every file below `root`, by `/`-separated path.
pub fn tree_digests(root: &Path) -> std::collections::BTreeMap<String, [u8; 32]> {
    tree_state(root)
        .into_iter()
        .filter(|(_, (directory, _, _))| !directory)
        .map(|(path, _)| {
            let native = path
                .split('/')
                .fold(root.to_owned(), |at, name| at.join(name));
            let digest = file_digest(&native);
            (path, digest)
        })
        .collect()
}

/// The native path of a `/`-separated path below `root`.
pub fn at(root: &Path, path: &str) -> PathBuf {
    path.split('/')
        .fold(root.to_owned(), |at, name| at.join(name))
}

/// Records one M2 case's verdict (PASS, FAIL or UNSUPPORTED) with what the
/// user sees. Printed, and appended to the file `LAYERFS_PROJFS_M2_RESULTS`
/// names, from which the report's table is made.
pub fn verdict(case: &str, verdict: &str, detail: &str) {
    use std::io::Write;
    let line = format!("{case}\t{verdict}\t{detail}\n");
    print!("M2\t{line}");
    if let Some(path) = std::env::var_os("LAYERFS_PROJFS_M2_RESULTS") {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(line.as_bytes()))
            .expect("M2 results file");
    }
}
