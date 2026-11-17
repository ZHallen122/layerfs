//! The change index: every path the file system reported as changed since
//! the mount's last Commit, in memory and appended to one local file.
//!
//! ProjFS writes go straight to NTFS; the provider only hears about them.
//! The journal is what turns those notifications into the set of paths a
//! status or Commit must look at, so neither walks the tree. It is local,
//! disposable state: appended without sync, and rebuilt from the disk by a
//! rescan when a provider died with notifications unrecorded.
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One reported rename, in the spelling the application used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rename {
    pub from: String,
    pub to: String,
    pub directory: bool,
    pub sequence: u64,
}

/// One reported hard link: `link` is a new name of the file at `path`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Link {
    pub path: String,
    pub link: String,
    pub sequence: u64,
}

/// A copy of the journal at one moment.
#[derive(Clone, Debug, Default)]
pub struct Recorded {
    /// Touched paths, `/`-separated as typed, with the sequence of the
    /// latest report for each.
    pub touched: BTreeMap<String, u64>,
    pub renames: Vec<Rename>,
    pub links: Vec<Link>,
    /// The sequence of the newest record; 0 for an empty journal.
    pub sequence: u64,
}

struct Inner {
    recorded: Recorded,
    file: Option<File>,
    path: Option<PathBuf>,
}

/// The journal of one mount.
pub struct Journal(Mutex<Inner>);

fn slashed(path: &str) -> String {
    path.replace('\\', "/")
}

impl Journal {
    /// A journal kept in memory only.
    pub fn memory() -> Self {
        Self(Mutex::new(Inner {
            recorded: Recorded::default(),
            file: None,
            path: None,
        }))
    }

    /// Opens the journal file at `path`, replaying what it holds.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut recorded = Recorded::default();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let fields: Vec<&str> = line.split('\t').collect();
                recorded.sequence += 1;
                let sequence = recorded.sequence;
                match fields.as_slice() {
                    ["T", path] => {
                        recorded.touched.insert((*path).to_owned(), sequence);
                    }
                    ["R", kind, from, to] => {
                        recorded.touched.insert((*from).to_owned(), sequence);
                        recorded.touched.insert((*to).to_owned(), sequence);
                        recorded.renames.push(Rename {
                            from: (*from).to_owned(),
                            to: (*to).to_owned(),
                            directory: *kind == "D",
                            sequence,
                        });
                    }
                    ["L", path, link] => {
                        recorded.touched.insert((*link).to_owned(), sequence);
                        recorded.links.push(Link {
                            path: (*path).to_owned(),
                            link: (*link).to_owned(),
                            sequence,
                        });
                    }
                    // A torn last line of a killed provider is dropped.
                    _ => recorded.sequence -= 1,
                }
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self(Mutex::new(Inner {
            recorded,
            file: Some(file),
            path: Some(path.to_owned()),
        })))
    }

    fn record(&self, line: String, apply: impl FnOnce(&mut Recorded, u64)) {
        let mut inner = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        inner.recorded.sequence += 1;
        let sequence = inner.recorded.sequence;
        apply(&mut inner.recorded, sequence);
        if let Some(file) = inner.file.as_mut() {
            // Disposable local state: appended, never synced. A lost line
            // is recovered by a rescan.
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// Records that the item at `path` was created, changed or removed.
    pub fn touch(&self, path: &str) {
        let path = slashed(path);
        if path.is_empty() {
            return;
        }
        self.record(format!("T\t{path}\n"), |recorded, sequence| {
            recorded.touched.insert(path, sequence);
        });
    }

    /// Records a rename. A side outside the mount arrives empty and is
    /// recorded as a plain change of the other side.
    pub fn rename(&self, from: &str, to: &str, directory: bool) {
        let (from, to) = (slashed(from), slashed(to));
        if from.is_empty() || to.is_empty() {
            self.touch(if from.is_empty() { &to } else { &from });
            return;
        }
        let kind = if directory { "D" } else { "F" };
        self.record(
            format!("R\t{kind}\t{from}\t{to}\n"),
            |recorded, sequence| {
                recorded.touched.insert(from.clone(), sequence);
                recorded.touched.insert(to.clone(), sequence);
                recorded.renames.push(Rename {
                    from,
                    to,
                    directory,
                    sequence,
                });
            },
        );
    }

    /// Records a new hard link `link` to the file at `path`.
    pub fn link(&self, path: &str, link: &str) {
        let (path, link) = (slashed(path), slashed(link));
        if link.is_empty() {
            return;
        }
        self.record(format!("L\t{path}\t{link}\n"), |recorded, sequence| {
            recorded.touched.insert(link.clone(), sequence);
            recorded.links.push(Link {
                path,
                link,
                sequence,
            });
        });
    }

    /// The journal now.
    pub fn recorded(&self) -> Recorded {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .recorded
            .clone()
    }

    /// Forgets every record up to and including `sequence`, except the
    /// touched paths `keep` selects, and rewrites the file with what remains.
    /// Records newer than `sequence` were reported after the caller read
    /// the journal and stay for the next status or Commit.
    pub fn forget_through(
        &self,
        sequence: u64,
        keep: &dyn Fn(&str) -> bool,
    ) -> std::io::Result<()> {
        let mut inner = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        let recorded = &mut inner.recorded;
        recorded
            .touched
            .retain(|path, at| *at > sequence || keep(path));
        recorded.renames.retain(|rename| rename.sequence > sequence);
        recorded.links.retain(|link| link.sequence > sequence);
        let Some(path) = inner.path.clone() else {
            return Ok(());
        };
        let mut text = String::new();
        let mut lines: Vec<(u64, String)> = Vec::new();
        let recorded = &inner.recorded;
        let renamed: Vec<&str> = recorded
            .renames
            .iter()
            .flat_map(|rename| [rename.from.as_str(), rename.to.as_str()])
            .collect();
        for (path, at) in &recorded.touched {
            if !renamed.contains(&path.as_str()) {
                lines.push((*at, format!("T\t{path}\n")));
            }
        }
        for rename in &recorded.renames {
            let kind = if rename.directory { "D" } else { "F" };
            lines.push((
                rename.sequence,
                format!("R\t{kind}\t{}\t{}\n", rename.from, rename.to),
            ));
        }
        for link in &recorded.links {
            lines.push((link.sequence, format!("L\t{}\t{}\n", link.path, link.link)));
        }
        lines.sort();
        lines.iter().for_each(|(_, line)| text.push_str(line));
        // Replace by rewrite: the journal is this provider's alone.
        inner.file = None;
        std::fs::write(&path, text)?;
        inner.file = Some(OpenOptions::new().append(true).open(&path)?);
        Ok(())
    }
}
