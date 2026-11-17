//! The per-project parse cache (F19 "Parse cache"): each file's parse, keyed by its content
//! hash and the parser version, in an append-only log of checksummed frames shared by every
//! agent of the project. After A.TXY's write-ahead log: a torn or corrupt frame ends the read,
//! and the files it held are parsed again.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::extract::FileParse;
use crate::language::Language;

/// Bump when extraction changes, so every cached parse is redone.
pub const PARSER_VERSION: u32 = 8;

#[derive(Serialize, Deserialize)]
struct Frame {
    key: String,
    parse: FileParse,
}

type Entries = HashMap<String, (Arc<FileParse>, u64)>;

pub struct Cache {
    log: PathBuf,
    entries: Entries,
    verified_len: u64,
    observed_len: u64,
}

/// The cache key of a file's parse.
pub fn key(hash: &str, language: Language) -> String {
    // The build's version too: a grammar upgrade changes parses without touching
    // `PARSER_VERSION` (Graphify #4236), and a release re-parses once.
    format!("{}{language:?}:{hash}", version())
}

fn version() -> String {
    format!("{PARSER_VERSION}:{}:", env!("CARGO_PKG_VERSION"))
}

impl Cache {
    /// The cache under `folder`. A missing or unreadable log is an empty cache.
    pub fn open(folder: &Path) -> Self {
        let log = folder.join("parses.log");
        let mut cache = Self {
            log,
            entries: HashMap::new(),
            verified_len: 0,
            observed_len: 0,
        };
        cache.reload();
        cache
    }

    /// Reads the log again, picking up what other agents appended since it was opened.
    pub fn reload(&mut self) {
        if self.size() != self.observed_len
            && let Ok(bytes) = std::fs::read(&self.log)
        {
            (self.entries, self.verified_len) = read_frames(&bytes);
            self.observed_len = bytes.len() as u64;
        }
    }

    pub fn size(&self) -> u64 {
        std::fs::metadata(&self.log).map_or(0, |metadata| metadata.len())
    }

    /// Serializes cache misses, log writes and compaction across workspaces.
    pub fn build_lock(&self) -> Result<File, Error> {
        if let Some(folder) = self.log.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.log.with_extension("build.lock"))?;
        lock.lock()?;
        Ok(lock)
    }

    pub fn get(&self, key: &str) -> Option<Arc<FileParse>> {
        self.entries.get(key).map(|(parse, _)| parse.clone())
    }

    /// Appends new parses while the caller holds the build lock.
    pub fn append(&mut self, parses: &[(String, Arc<FileParse>)]) -> Result<(), Error> {
        if parses.is_empty() {
            return Ok(());
        }
        self.reload();
        let mut log = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.log)?;
        log.set_len(self.verified_len)?;
        log.seek(std::io::SeekFrom::End(0))?;
        for (key, parse) in parses {
            if self.entries.contains_key(key) {
                continue;
            }
            let frame = frame(key, parse)?;
            log.write_all(&frame)?;
            self.entries
                .insert(key.clone(), (parse.clone(), frame.len() as u64));
            self.verified_len += frame.len() as u64;
        }
        log.flush()?;
        self.observed_len = self.verified_len;
        Ok(())
    }

    /// Rewrites the log with unique parses for the current parser version.
    pub fn compact(&mut self) -> Result<(), Error> {
        let live_size: u64 = self.entries.values().map(|(_, size)| *size).sum();
        if self.observed_len <= live_size * 2 {
            return Ok(());
        }
        let mut bytes = Vec::new();
        for (key, (parse, _)) in &self.entries {
            bytes.extend(frame(key, parse)?);
        }
        let temporary = self.log.with_extension("log.tmp");
        std::fs::write(&temporary, &bytes)?;
        std::fs::rename(&temporary, &self.log)?;
        self.observed_len = bytes.len() as u64;
        self.verified_len = self.observed_len;
        Ok(())
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
fn read_frames(bytes: &[u8]) -> (Entries, u64) {
    let mut entries = HashMap::new();
    let version = version();
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
        if frame.key.starts_with(&version) {
            entries.insert(frame.key, (Arc::new(frame.parse), (length + 8) as u64));
        }
        offset += length + 8;
    }
    (entries, offset as u64)
}
