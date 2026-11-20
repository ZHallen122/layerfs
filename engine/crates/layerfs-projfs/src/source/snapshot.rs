//! One committed LayerFS tree as a source: the immutable root a Workspace
//! is bound to, read through the Store's own ports. Nothing is copied at
//! mount; a directory or a file range is read when the file system asks.
use super::{attribute, ticks, Entry, Listing, Source, SourceError};
use layerfs_content::compare::TreeTable;
use layerfs_content::filesystem::attributes::{read_portable, AttributeReadWork};
use layerfs_content::filesystem::directory::read::{list_after, DirectoryReadWork};
use layerfs_content::filesystem::path::PathName;
use layerfs_content::filesystem::sorted::DirectoryRoot;
use layerfs_content::filesystem::FilesystemRootId;
use layerfs_content::object::inode_leaf::InodeKind;
use layerfs_content::ObjectId;
use layerfs_store_service::store::{Store, StorePorts};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Names read per directory page.
const PAGE_ENTRIES: usize = 256;
const PAGE_BYTES: usize = 64 * 1024;

/// A source over the saved tree `root` of the stack with allocation scope
/// `scope`.
pub struct SnapshotSource {
    store: Arc<Store>,
    scope: ObjectId,
    root: ObjectId,
    tree: TreeTable,
    /// Directory serials by path, so a deep path is walked once.
    serials: Mutex<HashMap<String, u64>>,
}

fn failed(error: impl std::fmt::Display) -> SourceError {
    SourceError::Failed(error.to_string())
}

/// Why Windows cannot show an entry with this name, if it cannot.
pub fn unrepresentable(name: &str) -> Option<&'static str> {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if name.is_empty() {
        return Some("empty name");
    }
    if name
        .chars()
        .any(|c| c < ' ' || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
    {
        return Some("a character Windows names cannot hold");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("Windows names cannot end in a dot or a space");
    }
    let stem = name.split('.').next().unwrap_or(name);
    if RESERVED.iter().any(|word| stem.eq_ignore_ascii_case(word)) {
        return Some("a reserved Windows device name");
    }
    if name.encode_utf16().count() > 255 {
        return Some("longer than 255 UTF-16 units");
    }
    None
}

impl SnapshotSource {
    /// Opens the tree `root`; reads its root object only.
    pub fn open(store: &Arc<Store>, scope: ObjectId, root: ObjectId) -> Result<Self, SourceError> {
        let ports = store.ports(scope);
        let tree = TreeTable::open(ports.as_ref(), FilesystemRootId(root)).map_err(failed)?;
        Ok(Self {
            store: store.clone(),
            scope,
            root,
            tree,
            serials: Mutex::new(HashMap::new()),
        })
    }

    /// The tree's root object.
    pub fn root(&self) -> ObjectId {
        self.root
    }

    fn serial(&self, ports: &StorePorts, directory: &str) -> Result<u64, SourceError> {
        if directory.is_empty() {
            return Ok(self.tree.root_serial());
        }
        if let Some(serial) = self
            .serials
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(directory)
        {
            return Ok(*serial);
        }
        let (parent, name) = directory.rsplit_once('/').unwrap_or(("", directory));
        let parent = self.serial(ports, parent)?;
        let name = PathName::new(name).map_err(|_| SourceError::NotFound)?;
        let serial = self
            .tree
            .lookup(ports, parent, &name)
            .map_err(failed)?
            .ok_or(SourceError::NotFound)?;
        self.serials
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(directory.to_owned(), serial);
        Ok(serial)
    }
}

impl Source for SnapshotSource {
    fn list(&self, directory: &str) -> Result<Listing, SourceError> {
        // Fresh demand custody for this one read, as every Store call takes.
        let ports = self.store.ports(self.scope);
        let serial = self.serial(&ports, directory)?;
        let value = self
            .tree
            .value(ports.as_ref(), serial)
            .map_err(failed)?
            .filter(|value| value.kind == InodeKind::Directory)
            .ok_or(SourceError::NotFound)?;
        let mut listing = Listing::default();
        let mut after: Option<PathName> = None;
        loop {
            let page = list_after(
                ports.as_ref(),
                DirectoryRoot(value.content_root),
                after.as_ref(),
                PAGE_ENTRIES,
                PAGE_BYTES,
                &mut DirectoryReadWork::default(),
            )
            .map_err(failed)?;
            let serials: Vec<u64> = page.entries.iter().map(|(_, serial)| *serial).collect();
            let values = self.tree.values(ports.as_ref(), &serials).map_err(failed)?;
            let mut files = Vec::new();
            let mut entries = Vec::new();
            for ((name, _), value) in page.entries.iter().zip(values) {
                let value = value.ok_or_else(|| failed("a bound name has no inode"))?;
                let text = name.as_str().to_owned();
                if text.is_empty() {
                    listing.unprojected.push((
                        String::from_utf8_lossy(name.as_bytes()).into_owned(),
                        "name is not UTF-8",
                    ));
                    continue;
                }
                if let Some(reason) = unrepresentable(&text) {
                    listing.unprojected.push((text, reason));
                    continue;
                }
                if value.kind == InodeKind::Symlink {
                    listing.unprojected.push((text, "symbolic link"));
                    continue;
                }
                let metadata = read_portable(
                    ports.as_ref(),
                    value.metadata_root,
                    value.kind,
                    &mut AttributeReadWork::default(),
                )
                .map_err(failed)?;
                let folder = value.kind == InodeKind::Directory;
                if !folder {
                    files.push((entries.len(), value.content_root));
                }
                let read_only = !folder && metadata.mode & 0o222 == 0;
                entries.push(Entry {
                    name: text,
                    directory: folder,
                    size: 0,
                    written: ticks(metadata.mtime_seconds, metadata.mtime_nanoseconds),
                    attributes: if folder {
                        attribute::DIRECTORY
                    } else {
                        attribute::ARCHIVE | if read_only { attribute::READ_ONLY } else { 0 }
                    },
                    content: if folder {
                        Vec::new()
                    } else {
                        value.content_root.as_bytes().to_vec()
                    },
                });
            }
            // A file's length is Store metadata: no payload is read for it.
            let ids: Vec<ObjectId> = files.iter().map(|(_, id)| *id).collect();
            let lengths = ports.file_lengths(&ids).map_err(failed)?;
            if lengths.len() != ids.len() {
                return Err(failed("file length cardinality"));
            }
            for ((index, _), length) in files.iter().zip(lengths) {
                entries[*index].size = length;
            }
            listing.entries.extend(entries);
            match page.continuation {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        Ok(listing)
    }

    fn read(
        &self,
        _path: &str,
        content: &[u8],
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), SourceError> {
        let root = content
            .get(..32)
            .and_then(|bytes| ObjectId::from_bytes(bytes).ok())
            .ok_or_else(|| failed("a placeholder without a content identity"))?;
        let ports = self.store.ports(self.scope);
        let length = buffer.len() as u64;
        let mut sink: &mut [u8] = buffer;
        layerfs_store_service::telemetry::timed("projfs.read", |timing| {
            layerfs_content::file::read_range(
                ports.as_ref(),
                root,
                offset..offset + length,
                &mut sink,
                timing.child("content.read"),
            )
        })
        .map_err(failed)?;
        if !sink.is_empty() {
            return Err(failed("the Store returned fewer bytes than the file holds"));
        }
        Ok(())
    }
}
