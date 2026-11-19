//! The local layer of a mount: what is actually on the NTFS disk below the
//! virtualization root, read while the provider shows its own process no
//! projected entry. Reading it materializes nothing.
use crate::sys;
use std::collections::HashMap;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One item on the local disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalItem {
    pub name: String,
    pub directory: bool,
    pub size: u64,
    /// Last write time, in 100 ns ticks since 1601.
    pub written: i64,
    pub attributes: u32,
}

impl LocalItem {
    /// Whether this is a junction, a symbolic link or another reparse point
    /// an application made. ProjFS's own are hidden from every process, so
    /// a placeholder never shows as one.
    pub fn link(&self) -> bool {
        self.attributes & crate::ntfs::attribute::REPARSE_POINT != 0
    }
}

struct Listed {
    wide: Vec<u16>,
    item: LocalItem,
}

/// A set of names looked up as NTFS compares them: sorted once, so that
/// comparing two directories' names does not grow with their product.
pub struct Names(Vec<Vec<u16>>);

impl Names {
    pub fn new<'a>(names: impl IntoIterator<Item = &'a String>) -> Self {
        let mut wide: Vec<Vec<u16>> = names.into_iter().map(sys::wide).collect();
        wide.sort_by(|left, right| sys::compare(left, right));
        Self(wide)
    }

    /// Whether the set holds `name`, given as `sys::wide` spells it.
    pub fn has(&self, name: &[u16]) -> bool {
        self.0
            .binary_search_by(|held| sys::compare(held, name))
            .is_ok()
    }
}

/// A reader of the local layer with the directories it listed so far.
///
/// The caller holds the provider's `LocalOnly` guard for its lifetime.
pub struct Local {
    root: PathBuf,
    directories: HashMap<String, Option<Arc<Vec<Listed>>>>,
}

/// The native path of a `/`-separated path below `root`, spelled verbatim
/// so it may exceed 260 characters.
pub fn native(root: &Path, path: &str) -> PathBuf {
    let joined = path
        .split('/')
        .filter(|name| !name.is_empty())
        .fold(root.to_owned(), |at, name| at.join(name));
    verbatim(&joined)
}

/// `path` with the `\\?\` prefix that lifts the 260-character limit.
pub fn verbatim(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if text.starts_with(r"\\?\") || !path.is_absolute() {
        return path.to_owned();
    }
    match text.strip_prefix(r"\\") {
        Some(share) => PathBuf::from(format!(r"\\?\UNC\{share}")),
        None => PathBuf::from(format!(r"\\?\{text}")),
    }
}

impl Local {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            directories: HashMap::new(),
        }
    }

    /// The mount's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn listed(&mut self, directory: &str) -> std::io::Result<Option<Arc<Vec<Listed>>>> {
        if let Some(known) = self.directories.get(directory) {
            return Ok(known.clone());
        }
        let read = match std::fs::read_dir(native(&self.root, directory)) {
            Ok(read) => Some(read),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        let listed = match read {
            None => None,
            Some(read) => {
                let mut items = Vec::new();
                for entry in read {
                    let entry = entry?;
                    let metadata = entry.metadata()?;
                    // A name that is not UTF-16 cannot be committed; it is
                    // listed lossily so a status can still name it.
                    let name = entry.file_name().to_string_lossy().into_owned();
                    items.push(Listed {
                        wide: sys::wide(entry.file_name()),
                        item: LocalItem {
                            name,
                            directory: metadata.is_dir(),
                            size: if metadata.is_dir() {
                                0
                            } else {
                                metadata.file_size()
                            },
                            written: metadata.last_write_time() as i64,
                            attributes: metadata.file_attributes(),
                        },
                    });
                }
                items.sort_by(|left, right| sys::compare(&left.wide, &right.wide));
                Some(Arc::new(items))
            }
        };
        self.directories
            .insert(directory.to_owned(), listed.clone());
        Ok(listed)
    }

    /// The items on disk in the directory at `directory` (its on-disk
    /// spelling); `None` when no such directory is on disk.
    pub fn list(&mut self, directory: &str) -> std::io::Result<Option<Vec<LocalItem>>> {
        Ok(self
            .listed(directory)?
            .map(|items| items.iter().map(|listed| listed.item.clone()).collect()))
    }

    /// The on-disk item a typed path (any case, either separator) names,
    /// with its on-disk spelling. `None` when nothing is on disk there.
    pub fn find(&mut self, typed: &str) -> std::io::Result<Option<(String, LocalItem)>> {
        let mut at = String::new();
        let mut found = None;
        for name in crate::provider::tree::names(typed) {
            let Some(items) = self.listed(&at)? else {
                return Ok(None);
            };
            let wanted = sys::wide(name);
            let Ok(index) = items.binary_search_by(|listed| sys::compare(&listed.wide, &wanted))
            else {
                return Ok(None);
            };
            let item = items[index].item.clone();
            at = crate::provider::tree::join(&at, &item.name);
            found = Some(item);
        }
        Ok(found.map(|item| (at, item)))
    }

    /// The ProjFS state bits of the on-disk item at `path`.
    pub fn state(&self, path: &str) -> Result<Option<i32>, sys::Error> {
        sys::disk_state(&native(&self.root, path))
    }
}
