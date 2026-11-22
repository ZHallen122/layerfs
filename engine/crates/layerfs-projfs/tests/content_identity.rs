//! A placeholder's content identity reaches the source exactly as the
//! source gave it, even when it ends in zero bytes. (Found by the benchmark:
//! one LayerFS object identity in 256 ends in a zero byte, and trimming
//! "padding" made those files unreadable.)
#![cfg(windows)]

mod support;

use layerfs_projfs::source::{attribute, Entry, Listing, Source, SourceError};
use layerfs_projfs::Projection;
use std::sync::{Arc, Mutex};

const BYTES: &[u8] = b"identity kept";

struct OneFile {
    identity: Vec<u8>,
    asked: Mutex<Vec<Vec<u8>>>,
}

impl Source for OneFile {
    fn list(&self, directory: &str) -> Result<Listing, SourceError> {
        if !directory.is_empty() {
            return Err(SourceError::NotFound);
        }
        Ok(Listing {
            entries: vec![Entry {
                name: "file.bin".to_owned(),
                directory: false,
                size: BYTES.len() as u64,
                written: 132_000_000_000_000_000,
                attributes: attribute::ARCHIVE,
                content: self.identity.clone(),
            }],
            unprojected: Vec::new(),
        })
    }
    fn read(
        &self,
        _path: &str,
        content: &[u8],
        offset: u64,
        buffer: &mut [u8],
    ) -> Result<(), SourceError> {
        self.asked.lock().expect("lock").push(content.to_vec());
        if !content.starts_with(&self.identity) {
            return Err(SourceError::Failed("another identity".to_owned()));
        }
        let from = offset as usize;
        buffer.copy_from_slice(&BYTES[from..from + buffer.len()]);
        Ok(())
    }
}

#[test]
fn an_identity_ending_in_zero_bytes_is_handed_back_whole() {
    let home = support::Scratch::new("identity");
    // 32 bytes, as a LayerFS object identity, whose last three are zero.
    let mut identity = vec![0xAB; 32];
    identity[29..].fill(0);
    let source = Arc::new(OneFile {
        identity: identity.clone(),
        asked: Mutex::new(Vec::new()),
    });
    let mounted = Projection::mount(
        &home.path().join("root"),
        &home.path().join("state"),
        source.clone(),
        0,
    )
    .expect("mount");
    assert_eq!(
        std::fs::read(home.path().join("root").join("file.bin")).expect("read"),
        BYTES
    );
    let asked = source.asked.lock().expect("lock").clone();
    assert!(!asked.is_empty());
    assert!(asked.iter().all(|content| content[..32] == identity[..]));
    assert_eq!(mounted.provider().served().failures, 0);
    mounted.discard().expect("discard");
}
