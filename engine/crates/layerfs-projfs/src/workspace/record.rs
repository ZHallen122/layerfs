//! The mount's own record in its state directory: which project and Branch
//! the directory is a Workspace of, and the base tree its placeholders
//! were projected from. Local, disposable state.
use std::path::Path;

/// What a state directory records about its Workspace.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Record {
    pub project: String,
    pub branch: String,
    /// Hex identity of the base tree's root.
    pub root: String,
    /// Hex identity of the base Commit; empty on a Branch without one.
    pub head: String,
}

impl Record {
    pub fn read(state: &Path) -> Result<Self, String> {
        let path = state.join("workspace");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("no Workspace record at {}: {error}", path.display()))?;
        let mut record = Self::default();
        for line in text.lines() {
            match line.split_once('=') {
                Some(("project", value)) => record.project = value.to_owned(),
                Some(("branch", value)) => record.branch = value.to_owned(),
                Some(("root", value)) => record.root = value.to_owned(),
                Some(("head", value)) => record.head = value.to_owned(),
                _ => {}
            }
        }
        if record.project.is_empty() || record.branch.is_empty() || record.root.is_empty() {
            return Err(format!("incomplete Workspace record at {}", path.display()));
        }
        Ok(record)
    }

    pub fn write(&self, state: &Path) -> Result<(), String> {
        let path = state.join("workspace");
        std::fs::write(
            &path,
            format!(
                "project={}\nbranch={}\nroot={}\nhead={}\n",
                self.project, self.branch, self.root, self.head
            ),
        )
        .map_err(|error| format!("Workspace record {}: {error}", path.display()))
    }
}
