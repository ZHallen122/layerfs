//! The paths a merge could not bring over: items on disk that still show an
//! older tree than the one the engine installed, because the step after a
//! merge was refused there (another process held the file, ProjFS would not
//! update the placeholder).
//!
//! Such a path must not be forgotten. Nothing in the change journal marks
//! it, so a status would call the directory clean while the file is old,
//! and an edit of the old file would then be committed over the other
//! Workspace's change with no Conflict. So each path is kept in the state
//! directory with the tree it still shows and the tree it has to reach, is
//! tried again at every status and Commit, and is named by status until it
//! is brought over.
use std::collections::BTreeMap;
use std::path::Path;

/// One path that is behind: hex identities of the tree the item on disk
/// still shows and of the tree it has to be brought to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Behind {
    pub path: String,
    pub shown: String,
    pub target: String,
}

/// The paths of one Workspace that are behind.
#[derive(Clone, Debug, Default)]
pub struct Unfollowed {
    /// By the path as NTFS compares it.
    entries: BTreeMap<String, Behind>,
}

impl Unfollowed {
    /// Reads the list in `state`; an absent or unreadable one is empty.
    pub fn load(state: &Path) -> Self {
        let mut list = Self::default();
        let Ok(text) = std::fs::read_to_string(state.join("behind")) else {
            return list;
        };
        for line in text.lines() {
            let fields: Vec<&str> = line.splitn(3, '\t').collect();
            if let [shown, target, path] = fields.as_slice() {
                list.add(path, shown, target);
            }
        }
        list
    }

    /// Writes the list beside the old one and renames it in.
    pub fn save(&self, state: &Path) -> std::io::Result<()> {
        let mut text = String::new();
        for behind in self.entries.values() {
            text.push_str(&behind.shown);
            text.push('\t');
            text.push_str(&behind.target);
            text.push('\t');
            text.push_str(&behind.path);
            text.push('\n');
        }
        let (path, beside) = (state.join("behind"), state.join("behind.new"));
        std::fs::write(&beside, text)?;
        std::fs::rename(beside, path)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn holds(&self, path: &str) -> bool {
        self.entries.contains_key(&path.to_lowercase())
    }

    /// Enters `path`. A path already in the list keeps the tree it shows:
    /// that is what is on disk, whatever was tried since.
    pub fn add(&mut self, path: &str, shown: &str, target: &str) {
        let behind = self
            .entries
            .entry(path.to_lowercase())
            .or_insert_with(|| Behind {
                path: path.to_owned(),
                shown: shown.to_owned(),
                target: String::new(),
            });
        target.clone_into(&mut behind.target);
    }

    /// Takes `path` and everything below it out of the list.
    pub fn remove(&mut self, path: &str) {
        let at = path.to_lowercase();
        self.entries.remove(&at);
        let below = format!("{at}/");
        self.entries.retain(|key, _| !key.starts_with(&below));
    }

    /// A later merge installed `target`: every path still behind has to
    /// reach that tree now.
    pub fn retarget(&mut self, target: &str) {
        for behind in self.entries.values_mut() {
            target.clone_into(&mut behind.target);
        }
    }

    /// The paths, as spelled when they were entered.
    pub fn paths(&self) -> Vec<String> {
        self.entries
            .values()
            .map(|behind| behind.path.clone())
            .collect()
    }

    /// The paths grouped by the pair of trees they are between.
    pub fn groups(&self) -> Vec<((String, String), Vec<String>)> {
        let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for behind in self.entries.values() {
            groups
                .entry((behind.shown.clone(), behind.target.clone()))
                .or_default()
                .push(behind.path.clone());
        }
        groups.into_iter().collect()
    }
}
