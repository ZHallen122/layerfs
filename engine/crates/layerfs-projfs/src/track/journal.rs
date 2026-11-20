//! The renames and hard links ProjFS reported since the mount's last
//! Commit, in memory and appended to one local file.
//!
//! Which items changed is read from the volume's change journal
//! (`watch.rs`), which names no path. These reports add the one thing it
//! cannot say: which old name a new one was. With them a move stays a move
//! in the status and keeps its identity in the Commit; without them (a
//! provider that died, a change made while none ran) the same change is
//! found as a deletion and an addition, and nothing is missed.
//!
//! Local, disposable state: appended without sync.
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

/// A copy of the reports at one moment.
#[derive(Clone, Debug, Default)]
pub struct Recorded {
    pub renames: Vec<Rename>,
    pub links: Vec<Link>,
    /// The sequence of the newest record; 0 when there is none.
    pub sequence: u64,
}

struct Inner {
    recorded: Recorded,
    file: Option<File>,
    path: Option<PathBuf>,
}

/// The reports of one mount.
pub struct Journal(Mutex<Inner>);

fn slashed(path: &str) -> String {
    path.replace('\\', "/")
}

fn line(rename: &Rename) -> String {
    let kind = if rename.directory { "D" } else { "F" };
    format!("R\t{kind}\t{}\t{}\n", rename.from, rename.to)
}

impl Journal {
    /// Reports kept in memory only.
    pub fn memory() -> Self {
        Self(Mutex::new(Inner {
            recorded: Recorded::default(),
            file: None,
            path: None,
        }))
    }

    /// Opens the file at `path`, replaying what it holds.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut recorded = Recorded::default();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let fields: Vec<&str> = line.split('\t').collect();
                let sequence = recorded.sequence + 1;
                match fields.as_slice() {
                    ["R", kind, from, to] => recorded.renames.push(Rename {
                        from: (*from).to_owned(),
                        to: (*to).to_owned(),
                        directory: *kind == "D",
                        sequence,
                    }),
                    ["L", path, link] => recorded.links.push(Link {
                        path: (*path).to_owned(),
                        link: (*link).to_owned(),
                        sequence,
                    }),
                    // A torn last line of a killed provider is dropped.
                    _ => continue,
                }
                recorded.sequence = sequence;
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
            // turns a move into a deletion and an addition.
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// Records a rename. A side outside the mount arrives empty; such a
    /// move is an addition or a deletion and needs no record.
    pub fn rename(&self, from: &str, to: &str, directory: bool) {
        let (from, to) = (slashed(from), slashed(to));
        if from.is_empty() || to.is_empty() {
            return;
        }
        let rename = Rename {
            from,
            to,
            directory,
            sequence: 0,
        };
        self.record(line(&rename), |recorded, sequence| {
            recorded.renames.push(Rename { sequence, ..rename });
        });
    }

    /// Records a new hard link `link` to the file at `path`.
    pub fn link(&self, path: &str, link: &str) {
        let (path, link) = (slashed(path), slashed(link));
        if link.is_empty() {
            return;
        }
        self.record(format!("L\t{path}\t{link}\n"), |recorded, sequence| {
            recorded.links.push(Link {
                path,
                link,
                sequence,
            });
        });
    }

    /// The reports now.
    pub fn recorded(&self) -> Recorded {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .recorded
            .clone()
    }

    /// Forgets every record up to and including `sequence` and rewrites the
    /// file with what remains: reports that arrived after the caller read
    /// them stay for the next status or Commit.
    pub fn forget_through(&self, sequence: u64) -> std::io::Result<()> {
        let mut inner = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        let recorded = &mut inner.recorded;
        recorded.renames.retain(|rename| rename.sequence > sequence);
        recorded.links.retain(|link| link.sequence > sequence);
        let Some(path) = inner.path.clone() else {
            return Ok(());
        };
        let mut lines: Vec<(u64, String)> = inner
            .recorded
            .renames
            .iter()
            .map(|rename| (rename.sequence, line(rename)))
            .collect();
        for link in &inner.recorded.links {
            lines.push((link.sequence, format!("L\t{}\t{}\n", link.path, link.link)));
        }
        lines.sort();
        let text: String = lines.into_iter().map(|(_, line)| line).collect();
        // Replace by rewrite: the file is this provider's alone.
        inner.file = None;
        std::fs::write(&path, text)?;
        inner.file = Some(OpenOptions::new().append(true).open(&path)?);
        Ok(())
    }
}
