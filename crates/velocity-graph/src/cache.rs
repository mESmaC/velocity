//! The per-project parse cache (F19 "Parse cache"): each file's parse, keyed by its content
//! hash and the parser version, in an append-only log of checksummed frames shared by every
//! agent of the project. After A.TXY's write-ahead log: a torn or corrupt frame ends the read,
//! and the files it held are parsed again.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::extract::FileParse;
use crate::language::Language;

/// Bump when extraction changes, so every cached parse is redone.
pub const PARSER_VERSION: u32 = 2;

/// The log is rewritten with only live entries once it passes this size and twice the live
/// size.
const COMPACT_AFTER: u64 = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Frame {
    key: String,
    parse: FileParse,
}

pub struct Cache {
    log: PathBuf,
    entries: HashMap<String, (Arc<FileParse>, u64)>,
}

/// The cache key of a file's parse.
pub fn key(hash: &str, language: Language) -> String {
    format!("{PARSER_VERSION}:{language:?}:{hash}")
}

impl Cache {
    /// The cache under `folder`. A missing or unreadable log is an empty cache.
    pub fn open(folder: &Path) -> Self {
        let log = folder.join("parses.log");
        let entries = std::fs::read(&log)
            .map(|bytes| read_frames(&bytes))
            .unwrap_or_default();
        Self { log, entries }
    }

    pub fn get(&self, key: &str) -> Option<Arc<FileParse>> {
        self.entries.get(key).map(|(parse, _)| parse.clone())
    }

    /// Appends new parses under the log's lock, so two agents' writes never interleave.
    pub fn append(&mut self, parses: &[(String, Arc<FileParse>)]) -> Result<(), Error> {
        if parses.is_empty() {
            return Ok(());
        }
        let mut bytes = Vec::new();
        for (key, parse) in parses {
            let frame = frame(key, parse)?;
            self.entries
                .insert(key.clone(), (parse.clone(), frame.len() as u64));
            bytes.extend(frame);
        }
        if let Some(folder) = self.log.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let lock = self.lock()?;
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)?;
        log.write_all(&bytes)?;
        log.flush()?;
        drop(lock);
        Ok(())
    }

    /// Rewrites the log with only the parses in `live` when it has grown well past them.
    pub fn compact(&mut self, live: &[String]) -> Result<(), Error> {
        let size = std::fs::metadata(&self.log).map_or(0, |metadata| metadata.len());
        let live_size: u64 = live
            .iter()
            .filter_map(|key| self.entries.get(key).map(|(_, size)| *size))
            .sum();
        if size < COMPACT_AFTER || size < live_size * 2 {
            return Ok(());
        }
        let mut bytes = Vec::new();
        for key in live {
            if let Some((parse, _)) = self.entries.get(key) {
                bytes.extend(frame(key, parse)?);
            }
        }
        let lock = self.lock()?;
        let temporary = self.log.with_extension("log.tmp");
        std::fs::write(&temporary, &bytes)?;
        std::fs::rename(&temporary, &self.log)?;
        drop(lock);
        Ok(())
    }

    /// An exclusive lock on a file beside the log, released when the returned file closes.
    fn lock(&self) -> Result<File, Error> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.log.with_extension("lock"))?;
        lock.lock()?;
        Ok(lock)
    }
}

/// `[length u32][crc32 u32][JSON]`, little-endian.
fn frame(key: &str, parse: &FileParse) -> Result<Vec<u8>, Error> {
    let body = serde_json::to_vec(&Frame {
        key: key.to_owned(),
        parse: parse.clone(),
    })?;
    let mut bytes = Vec::with_capacity(body.len() + 8);
    bytes.extend((body.len() as u32).to_le_bytes());
    bytes.extend(crc32fast::hash(&body).to_le_bytes());
    bytes.extend(body);
    Ok(bytes)
}

/// Every whole, valid frame up to the first torn or corrupt one.
fn read_frames(bytes: &[u8]) -> HashMap<String, (Arc<FileParse>, u64)> {
    let mut entries = HashMap::new();
    let mut offset = 0usize;
    while let Some(header) = bytes.get(offset..offset + 8) {
        let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let checksum = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let Some(body) = bytes.get(offset + 8..offset + 8 + length) else {
            break;
        };
        if crc32fast::hash(body) != checksum {
            break;
        }
        let Ok(frame) = serde_json::from_slice::<Frame>(body) else {
            break;
        };
        entries.insert(frame.key, (Arc::new(frame.parse), (length + 8) as u64));
        offset += length + 8;
    }
    entries
}
