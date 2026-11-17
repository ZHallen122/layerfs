//! The projected tree as the provider serves it: a source's directories,
//! ordered and looked up the way NTFS compares names.
use crate::source::{Entry, Source, SourceError};
use crate::sys;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};

/// Directories kept in memory at once; beyond it the cache starts over.
const DIRECTORIES: usize = 8192;

/// One entry with its NUL-terminated wide name.
pub struct Item {
    pub wide: Vec<u16>,
    pub entry: Entry,
}

/// One source directory in NTFS name order.
pub struct Directory {
    pub items: Vec<Item>,
}

impl Directory {
    /// The entry an application's spelling of a name selects.
    pub fn find(&self, typed: &str) -> Option<&Entry> {
        let wanted = sys::wide(typed);
        self.items
            .binary_search_by(|item| sys::compare(&item.wide, &wanted))
            .ok()
            .map(|index| &self.items[index].entry)
    }
}

/// What a path resolved to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Found {
    Root,
    Entry { path: String, entry: Entry },
}

impl Found {
    pub fn is_directory(&self) -> bool {
        match self {
            Self::Root => true,
            Self::Entry { entry, .. } => entry.directory,
        }
    }
    /// The source's spelling of the path; empty for the root.
    pub fn path(&self) -> &str {
        match self {
            Self::Root => "",
            Self::Entry { path, .. } => path,
        }
    }
}

/// The current source and its directories read so far.
pub struct Tree {
    source: RwLock<Arc<dyn Source>>,
    directories: Mutex<HashMap<String, Arc<Directory>>>,
    /// Paths a Windows mount cannot show, with the reason: a name equal to
    /// another but for case, or an entry the source reported unprojectable.
    hidden: Mutex<BTreeSet<(String, &'static str)>>,
}

/// Joins a `/`-separated path and a name.
pub fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    }
}

/// The names of a path an application typed, whichever separator it used.
pub fn names(path: &str) -> impl Iterator<Item = &str> {
    path.split(['\\', '/']).filter(|name| !name.is_empty())
}

impl Tree {
    pub fn new(source: Arc<dyn Source>) -> Self {
        Self {
            source: RwLock::new(source),
            directories: Mutex::new(HashMap::new()),
            hidden: Mutex::new(BTreeSet::new()),
        }
    }

    /// The source read now.
    pub fn source(&self) -> Arc<dyn Source> {
        self.source
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Serves `source` from now on and forgets every directory read before.
    pub fn replace(&self, source: Arc<dyn Source>) {
        *self
            .source
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = source;
        self.directories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
        self.hidden
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }

    /// Paths seen so far that the mount cannot show.
    pub fn hidden(&self) -> Vec<(String, &'static str)> {
        self.hidden
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// The directory at the source's spelling `path`, read once.
    pub fn directory(&self, path: &str) -> Result<Arc<Directory>, SourceError> {
        if let Some(found) = self
            .directories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(path)
        {
            return Ok(found.clone());
        }
        let listing = self.source().list(path)?;
        let mut items: Vec<Item> = listing
            .entries
            .into_iter()
            .map(|entry| Item {
                wide: sys::wide(&entry.name),
                entry,
            })
            .collect();
        // NTFS order first; names equal but for case fall back to byte
        // order, so the one shown is the same on every read.
        items.sort_by(|left, right| {
            sys::compare(&left.wide, &right.wide)
                .then_with(|| left.entry.name.cmp(&right.entry.name))
        });
        let mut hidden: Vec<(String, &'static str)> = listing
            .unprojected
            .into_iter()
            .map(|(name, reason)| (join(path, &name), reason))
            .collect();
        items.dedup_by(|later, first| {
            let same = sys::compare(&later.wide, &first.wide).is_eq();
            if same {
                hidden.push((
                    join(path, &later.entry.name),
                    "another name in this directory differs only by case",
                ));
            }
            same
        });
        if !hidden.is_empty() {
            self.hidden
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .extend(hidden);
        }
        let directory = Arc::new(Directory { items });
        let mut cache = self
            .directories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if cache.len() >= DIRECTORIES {
            cache.clear();
        }
        cache.insert(path.to_owned(), directory.clone());
        Ok(directory)
    }

    /// Resolves a path an application typed, in any case and with either
    /// separator, to the source's entry. `Ok(None)` when the source has no
    /// such entry.
    pub fn resolve(&self, typed: &str) -> Result<Option<Found>, SourceError> {
        let mut found = Found::Root;
        for name in names(typed) {
            if !found.is_directory() {
                return Ok(None);
            }
            let directory = match self.directory(found.path()) {
                Ok(directory) => directory,
                Err(SourceError::NotFound) => return Ok(None),
                Err(error) => return Err(error),
            };
            let Some(entry) = directory.find(name) else {
                return Ok(None);
            };
            found = Found::Entry {
                path: join(found.path(), &entry.name),
                entry: entry.clone(),
            };
        }
        Ok(Some(found))
    }
}
