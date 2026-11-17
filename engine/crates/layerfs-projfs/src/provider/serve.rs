//! The provider: answers the file system's requests from the tree and
//! records every change it is told about.
use super::tree::{Directory, Found, Tree};
use crate::source::{attribute, Entry, Source, SourceError};
use crate::sys::{self, BasicInfo, Fill, Handler, Hresult, Notification, Placeholder};
use crate::track::journal::Journal;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Requests served since the provider started.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Served {
    pub enumerations: u64,
    pub placeholders: u64,
    pub reads: u64,
    pub read_bytes: u64,
    pub notifications: u64,
    /// Requests answered with a failure other than "not found".
    pub failures: u64,
    /// The newest such failure: what was asked, for which path, and why.
    pub last_failure: Option<String>,
}

struct Session {
    /// `None` shows nothing: the provider's own local-only view.
    directory: Option<Arc<Directory>>,
    next: usize,
    pattern: Option<Vec<u16>>,
    started: bool,
}

/// One mount's provider state.
pub struct Provider {
    pub tree: Tree,
    pub journal: Arc<Journal>,
    sessions: Mutex<HashMap<u128, Session>>,
    /// While set, requests from the provider's own process see only what is
    /// on the local disk: no projected entry is listed or materialized.
    local_only: AtomicBool,
    enumerations: AtomicU64,
    placeholders: AtomicU64,
    reads: AtomicU64,
    read_bytes: AtomicU64,
    notifications: AtomicU64,
    failures: AtomicU64,
    last_failure: Mutex<Option<String>>,
}

/// Holds the provider's own process to the local-only view until dropped.
pub struct LocalOnly<'a>(&'a Provider);

impl Drop for LocalOnly<'_> {
    fn drop(&mut self) {
        self.0.local_only.store(false, Ordering::Release);
    }
}

/// The placeholder facts of a source entry.
pub fn basic_info(entry: &Entry) -> BasicInfo {
    BasicInfo {
        directory: entry.directory,
        size: entry.size,
        written: entry.written,
        attributes: if entry.attributes == 0 {
            attribute::ARCHIVE
        } else {
            entry.attributes
        },
    }
}

impl Provider {
    pub fn new(source: Arc<dyn Source>, journal: Arc<Journal>) -> Self {
        Self {
            tree: Tree::new(source),
            journal,
            sessions: Mutex::new(HashMap::new()),
            local_only: AtomicBool::new(false),
            enumerations: AtomicU64::new(0),
            placeholders: AtomicU64::new(0),
            reads: AtomicU64::new(0),
            read_bytes: AtomicU64::new(0),
            notifications: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            last_failure: Mutex::new(None),
        }
    }

    /// Records a source failure and answers it as a file system error.
    fn refused(&self, what: &str, path: &str, error: SourceError) -> Hresult {
        match error {
            SourceError::NotFound => sys::NOT_FOUND,
            SourceError::Failed(cause) => {
                self.failures.fetch_add(1, Ordering::Relaxed);
                *self
                    .last_failure
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) =
                    Some(format!("{what} {path}: {cause}"));
                sys::IO_ERROR
            }
        }
    }

    /// Shows the provider's own process only the local disk until the
    /// guard drops. Other processes are unaffected.
    pub fn local_only(&self) -> LocalOnly<'_> {
        self.local_only.store(true, Ordering::Release);
        LocalOnly(self)
    }

    fn hidden_from(&self, own: bool) -> bool {
        own && self.local_only.load(Ordering::Acquire)
    }

    pub fn served(&self) -> Served {
        Served {
            enumerations: self.enumerations.load(Ordering::Relaxed),
            placeholders: self.placeholders.load(Ordering::Relaxed),
            reads: self.reads.load(Ordering::Relaxed),
            read_bytes: self.read_bytes.load(Ordering::Relaxed),
            notifications: self.notifications.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            last_failure: self
                .last_failure
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone(),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<u128, Session>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Handler for Provider {
    fn start_enumeration(&self, id: u128, path: &str, own: bool) -> Result<(), Hresult> {
        self.enumerations.fetch_add(1, Ordering::Relaxed);
        let directory = if self.hidden_from(own) {
            None
        } else {
            match self
                .tree
                .resolve(path)
                .map_err(|e| self.refused("list", path, e))?
            {
                Some(found) if found.is_directory() => Some(
                    self.tree
                        .directory(found.path())
                        .map_err(|e| self.refused("list", path, e))?,
                ),
                // A directory that exists only locally projects nothing.
                _ => None,
            }
        };
        self.sessions().insert(
            id,
            Session {
                directory,
                next: 0,
                pattern: None,
                started: false,
            },
        );
        Ok(())
    }

    fn enumerate(
        &self,
        id: u128,
        restart: bool,
        pattern: Option<&[u16]>,
        fill: &mut dyn FnMut(&[u16], &BasicInfo) -> Fill,
    ) -> Result<(), Hresult> {
        // The session leaves the map while it is served, so a slow fill
        // never holds the lock other enumerations need.
        let mut session = self.sessions().remove(&id).ok_or(sys::INVALID_PARAMETER)?;
        if restart || !session.started {
            session.pattern = pattern.map(<[u16]>::to_vec);
            session.next = 0;
            session.started = true;
        }
        if let Some(directory) = session.directory.clone() {
            while let Some(item) = directory.items.get(session.next) {
                let wanted = session
                    .pattern
                    .as_ref()
                    .is_none_or(|pattern| sys::matches(&item.wide, pattern));
                if wanted && fill(&item.wide, &basic_info(&item.entry)) == Fill::Full {
                    break;
                }
                session.next += 1;
            }
        }
        self.sessions().insert(id, session);
        Ok(())
    }

    fn end_enumeration(&self, id: u128) {
        self.sessions().remove(&id);
    }

    fn placeholder(&self, path: &str, own: bool) -> Result<Placeholder, Hresult> {
        if self.hidden_from(own) {
            return Err(sys::NOT_FOUND);
        }
        self.placeholders.fetch_add(1, Ordering::Relaxed);
        match self
            .tree
            .resolve(path)
            .map_err(|e| self.refused("placeholder", path, e))?
        {
            Some(Found::Entry { path, entry }) => Ok(Placeholder {
                path: path.replace('/', "\\"),
                info: basic_info(&entry),
                content: entry.content,
            }),
            _ => Err(sys::NOT_FOUND),
        }
    }

    fn read(
        &self,
        path: &str,
        content: &[u8],
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), Hresult> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.read_bytes
            .fetch_add(buffer.len() as u64, Ordering::Relaxed);
        let source = self.tree.source();
        // A placeholder keeps the identity it was created with. Without
        // one, the path it has now is resolved to the source's spelling.
        // The identity is handed over whole, zero padding included: only the
        // source knows its width, and an identity may itself end in zeros.
        if content.iter().any(|byte| *byte != 0) {
            return source
                .read(&path.replace('\\', "/"), content, offset, buffer)
                .map_err(|e| self.refused("read", path, e));
        }
        match self
            .tree
            .resolve(path)
            .map_err(|e| self.refused("read", path, e))?
        {
            Some(Found::Entry { path, entry }) if !entry.directory => source
                .read(&path, &[], offset, buffer)
                .map_err(|e| self.refused("read", &path, e)),
            _ => Err(sys::NOT_FOUND),
        }
    }

    fn notify(&self, notification: Notification) -> Result<(), Hresult> {
        self.notifications.fetch_add(1, Ordering::Relaxed);
        match notification {
            Notification::Created { path, .. }
            | Notification::Overwritten { path }
            | Notification::ConvertingToFull { path }
            | Notification::Modified { path }
            | Notification::Deleted { path, .. } => self.journal.touch(&path),
            Notification::Renamed {
                from,
                to,
                directory,
            } => self.journal.rename(&from, &to, directory),
            Notification::HardLinked { path, link } => self.journal.link(&path, &link),
            // Nothing is denied: the Workspace is an ordinary directory.
            Notification::PreDelete { .. }
            | Notification::PreRename { .. }
            | Notification::PreHardLink { .. } => {}
        }
        Ok(())
    }
}
