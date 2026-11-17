//! A plain directory as a source: the standalone spike, with no Store.
//!
//! The directory is only ever opened for reading. It must not change while
//! it is mounted: nothing here detects that.
use super::{attribute, Entry, Listing, Source, SourceError};
use std::fs::File;
use std::os::windows::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Attribute bits passed through from the source directory.
const KEPT: u32 = attribute::READ_ONLY | attribute::HIDDEN | attribute::SYSTEM | attribute::ARCHIVE;
const REPARSE_POINT: u32 = 0x400;

/// A source that reads the directory at `root`.
pub struct DirectorySource {
    root: PathBuf,
    /// The file the last read opened, kept for the next window of it.
    open: Mutex<Option<(String, File)>>,
}

impl DirectorySource {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            open: Mutex::new(None),
        }
    }

    fn native(&self, path: &str) -> PathBuf {
        path.split('/')
            .filter(|name| !name.is_empty())
            .fold(self.root.clone(), |at, name| at.join(name))
    }
}

fn failed(error: std::io::Error) -> SourceError {
    if error.kind() == std::io::ErrorKind::NotFound {
        SourceError::NotFound
    } else {
        SourceError::Failed(error.to_string())
    }
}

impl Source for DirectorySource {
    fn list(&self, directory: &str) -> Result<Listing, SourceError> {
        let mut listing = Listing::default();
        for entry in std::fs::read_dir(self.native(directory)).map_err(failed)? {
            let entry = entry.map_err(failed)?;
            let Ok(name) = entry.file_name().into_string() else {
                listing.unprojected.push((
                    entry.file_name().to_string_lossy().into_owned(),
                    "name is not UTF-16",
                ));
                continue;
            };
            let metadata = entry.metadata().map_err(failed)?;
            if metadata.file_attributes() & REPARSE_POINT != 0 {
                listing.unprojected.push((name, "reparse point"));
                continue;
            }
            let folder = metadata.is_dir();
            let child = if directory.is_empty() {
                name.clone()
            } else {
                format!("{directory}/{name}")
            };
            listing.entries.push(Entry {
                directory: folder,
                size: if folder { 0 } else { metadata.file_size() },
                written: metadata.last_write_time() as i64,
                attributes: (metadata.file_attributes() & KEPT)
                    | if folder { attribute::DIRECTORY } else { 0 },
                // The path is the content identity when it fits.
                content: if child.len() <= 128 {
                    child.into_bytes()
                } else {
                    Vec::new()
                },
                name,
            });
        }
        Ok(listing)
    }

    fn read(
        &self,
        path: &str,
        content: &[u8],
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), SourceError> {
        let listed = std::str::from_utf8(content)
            .ok()
            .map(|text| text.trim_end_matches('\0'))
            .filter(|text| !text.is_empty())
            .unwrap_or(path);
        let mut open = self
            .open
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if open.as_ref().is_none_or(|(held, _)| held != listed) {
            *open = Some((
                listed.to_owned(),
                File::open(self.native(listed)).map_err(failed)?,
            ));
        }
        let file = &open.as_ref().expect("opened above").1;
        let mut filled = 0;
        while filled < buffer.len() {
            let read = file
                .seek_read(&mut buffer[filled..], offset + filled as u64)
                .map_err(failed)?;
            if read == 0 {
                return Err(SourceError::Failed(format!(
                    "{listed} ended at {} of a read to {}",
                    offset + filled as u64,
                    offset + buffer.len() as u64
                )));
            }
            filled += read;
        }
        Ok(())
    }
}
