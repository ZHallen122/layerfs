//! The record of committed files: what each full file on disk, and each
//! tombstone, was last committed as.
//!
//! A file this Workspace wrote stays an ordinary full file on disk after
//! its Commit, and ProjFS can say nothing more about it than "full". Without
//! a record, a full file is forever "possibly changed": it has to be read
//! and compared at every question, it cannot follow the Branch when a merge
//! changes it, and after a restart it looks like an uncommitted change. The
//! record holds, for each such file, the size and time it had when the
//! Commit read it and the identity of the content the Store then held at
//! its path. A file whose size and time are still those is the committed
//! one, as Git's index decides it.
//!
//! A tombstone is recorded only when it could not be cleared after its
//! deletion was committed, so that it is cleared when the name comes back.
//!
//! Local, disposable state: without it every full file is compared by its
//! bytes once.
use crate::track::local::LocalItem;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One full file as its last Commit stored it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stored {
    pub size: u64,
    /// Last write time, in 100 ns ticks since 1601.
    pub written: i64,
    /// The Store's identity of the bytes.
    pub content: Vec<u8>,
}

/// The record of one Workspace.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    /// By the path as NTFS compares it, with the path as spelled.
    files: BTreeMap<String, (String, Stored)>,
    tombstones: BTreeSet<String>,
}

fn folded(path: &str) -> String {
    path.to_lowercase()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len() / 2)
        .map(|at| u8::from_str_radix(text.get(at * 2..at * 2 + 2)?, 16).ok())
        .collect()
}

impl Ledger {
    /// Reads the record in `state`; an absent or unreadable one is empty.
    pub fn load(state: &Path) -> Self {
        let mut ledger = Self::default();
        let Ok(text) = std::fs::read_to_string(state.join("committed")) else {
            return ledger;
        };
        for line in text.lines() {
            let fields: Vec<&str> = line.splitn(5, '\t').collect();
            match fields.as_slice() {
                ["F", size, written, content, path] => {
                    let stored = (|| {
                        Some(Stored {
                            size: size.parse().ok()?,
                            written: written.parse().ok()?,
                            content: unhex(content)?,
                        })
                    })();
                    if let Some(stored) = stored {
                        ledger.record(path, stored);
                    }
                }
                ["T", path] => {
                    ledger.tombstones.insert((*path).to_owned());
                }
                _ => {}
            }
        }
        ledger
    }

    /// Writes the record beside the old one and renames it in.
    pub fn save(&self, state: &Path) -> std::io::Result<()> {
        let mut text = String::new();
        for (path, stored) in self.files.values() {
            text.push_str(&format!(
                "F\t{}\t{}\t{}\t{path}\n",
                stored.size,
                stored.written,
                hex(&stored.content)
            ));
        }
        for path in &self.tombstones {
            text.push_str(&format!("T\t{path}\n"));
        }
        let (path, beside) = (state.join("committed"), state.join("committed.new"));
        std::fs::write(&beside, text)?;
        std::fs::rename(beside, path)
    }

    /// Whether the full file `item` at `path` is the one last committed
    /// there: the same size and time.
    pub fn vouches(&self, path: &str, item: &LocalItem) -> bool {
        self.files
            .get(&folded(path))
            .is_some_and(|(_, stored)| stored.size == item.size && stored.written == item.written)
    }

    pub fn record(&mut self, path: &str, stored: Stored) {
        self.files.insert(folded(path), (path.to_owned(), stored));
    }

    /// Forgets `path` and everything below it.
    pub fn forget(&mut self, path: &str) {
        let (at, below) = (folded(path), format!("{}/", folded(path)));
        self.files
            .retain(|key, _| *key != at && !key.starts_with(&below));
    }

    /// The recorded files, each with its path as spelled.
    pub fn files(&self) -> Vec<(String, Stored)> {
        self.files.values().cloned().collect()
    }

    /// Remembers a committed deletion whose tombstone is still on disk.
    pub fn tombstone(&mut self, path: &str) {
        self.tombstones.insert(path.to_owned());
    }

    pub fn cleared(&mut self, path: &str) {
        self.tombstones.remove(path);
    }

    pub fn tombstones(&self) -> Vec<String> {
        self.tombstones.iter().cloned().collect()
    }
}
