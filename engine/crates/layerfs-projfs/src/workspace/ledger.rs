//! The record of committed files: what each full file on disk, and each
//! tombstone, was last committed as.
//!
//! A file this Workspace wrote stays an ordinary full file on disk after
//! its Commit, and ProjFS can say nothing more about it than "full". Without
//! a record, a full file is forever "possibly changed": it has to be read
//! and compared at every question, it cannot follow the Branch when a merge
//! changes it, and after a restart it looks like an uncommitted change. The
//! record holds, for each such file, the size, the time and the number of
//! its newest change journal record when the Commit read it. A file for
//! which all three are still the same is the committed one, and so equals
//! what the base held at its path when it was recorded. The journal number
//! is what makes that exact: every journaled change moves it, also one that
//! leaves size and time alone.
//!
//! The record is asked only where the change journal cannot answer for a
//! stretch of time, and when a merge has to decide whether a committed
//! file may be replaced. A file the journal marks is always compared.
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
    /// The number of the file's newest change journal record.
    pub change: i64,
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

impl Stored {
    /// Whether the file at `native`, listed as `item`, is still this one.
    pub fn is(&self, native: &Path, item: &LocalItem) -> bool {
        self.size == item.size
            && self.written == item.written
            && std::fs::File::open(native)
                .and_then(|file| crate::ntfs::last_change(&file))
                .is_ok_and(|change| change == self.change)
    }
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
                ["F", size, written, change, path] => {
                    let stored = (|| {
                        Some(Stored {
                            size: size.parse().ok()?,
                            written: written.parse().ok()?,
                            change: change.parse().ok()?,
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
                stored.size, stored.written, stored.change
            ));
        }
        for path in &self.tombstones {
            text.push_str(&format!("T\t{path}\n"));
        }
        let (path, beside) = (state.join("committed"), state.join("committed.new"));
        std::fs::write(&beside, text)?;
        std::fs::rename(beside, path)
    }

    /// What the file at `path` was last committed as.
    pub fn stored(&self, path: &str) -> Option<&Stored> {
        self.files.get(&folded(path)).map(|(_, stored)| stored)
    }

    /// Whether the full file `item` at `path`, read from the disk at
    /// `native`, is the one last committed there.
    pub fn vouches(&self, native: &Path, path: &str, item: &LocalItem) -> bool {
        self.stored(path)
            .is_some_and(|stored| stored.is(native, item))
    }

    pub fn record(&mut self, path: &str, stored: Stored) {
        self.files.insert(folded(path), (path.to_owned(), stored));
    }

    /// Forgets `path` and everything below it.
    pub fn forget(&mut self, path: &str) {
        let at = folded(path);
        self.files.remove(&at);
        // The keys below it are one run in the map's order.
        let below = format!("{at}/");
        let gone: Vec<String> = self
            .files
            .range(below.clone()..)
            .map(|(key, _)| key)
            .take_while(|key| key.starts_with(&below))
            .cloned()
            .collect();
        for key in gone {
            self.files.remove(&key);
        }
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
