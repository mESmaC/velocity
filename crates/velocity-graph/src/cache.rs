//! The per-project parse cache (F19 "Parse cache"): each file's parse, keyed by its content
//! hash and the parser version, in an append-only log of checksummed frames shared by every
//! agent of the project. After A.TXY's write-ahead log: a torn or corrupt frame ends the read,
//! and the files it held are parsed again.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::extract::FileParse;
use crate::language::Language;

/// Bump when extraction changes, so every cached parse is redone.
pub const PARSER_VERSION: u32 = 8;
const CACHE_LIMIT: u64 = 64 * 1024 * 1024;
const FRAME_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Serialize)]
struct Frame<'a> {
    key: &'a str,
    parse: &'a FileParse,
}

#[derive(Deserialize)]
struct FrameKey {
    key: String,
}

#[derive(Deserialize)]
struct FrameParse {
    parse: FileParse,
}

struct Entry {
    body: Vec<u8>,
    position: u64,
}

type Entries = HashMap<String, Entry>;

pub struct Cache {
    log: PathBuf,
    entries: Entries,
    verified_len: u64,
    observed_len: u64,
    live_bytes: u64,
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
            live_bytes: 0,
        };
        cache.reload();
        cache
    }

    /// Reads the log again, picking up what other agents appended since it was opened.
    pub fn reload(&mut self) {
        let size = self.size();
        if size != self.observed_len
            && let Ok(file) = File::open(&self.log)
        {
            self.entries.clear();
            self.live_bytes = 0;
            self.verified_len = 0;
            let version = version();
            let mut input = BufReader::new(file);
            while let Some(body) = read_frame(&mut input) {
                let Ok(FrameKey { key }) = serde_json::from_slice::<FrameKey>(&body) else {
                    break;
                };
                let position = self.verified_len;
                self.verified_len += body.len() as u64 + 8;
                if key.starts_with(&version) {
                    self.remember(key, body, position);
                }
            }
            self.observed_len = size;
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
        let entry = self.entries.get(key)?;
        let FrameParse { parse } = serde_json::from_slice(&entry.body).ok()?;
        Some(Arc::new(parse))
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
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
            let body = serde_json::to_vec(&Frame { key, parse })?;
            if body.len() > FRAME_LIMIT {
                continue;
            }
            write_frame(&mut log, &body)?;
            let position = self.verified_len;
            self.verified_len += body.len() as u64 + 8;
            self.remember(key.clone(), body, position);
        }
        log.flush()?;
        self.observed_len = self.verified_len;
        Ok(())
    }

    /// Rewrites the log with unique parses for the current parser version.
    pub fn compact(&mut self) -> Result<(), Error> {
        if self.observed_len <= self.live_bytes * 2 && self.observed_len <= CACHE_LIMIT * 2 {
            return Ok(());
        }
        let temporary = self.log.with_extension("log.tmp");
        let mut log = File::create(&temporary)?;
        let mut entries: Vec<_> = self.entries.values_mut().collect();
        entries.sort_unstable_by_key(|entry| entry.position);
        for entry in &entries {
            write_frame(&mut log, &entry.body)?;
        }
        log.flush()?;
        drop(log);
        std::fs::rename(&temporary, &self.log)?;
        self.observed_len = self.live_bytes;
        self.verified_len = self.observed_len;
        // Rebase offsets so newly appended entries remain newer after a rewrite.
        let mut offset = 0;
        for entry in entries {
            entry.position = offset;
            offset += entry.body.len() as u64 + 8;
        }
        Ok(())
    }

    fn remember(&mut self, key: String, body: Vec<u8>, position: u64) {
        self.live_bytes += body.len() as u64 + 8;
        if let Some(previous) = self.entries.insert(key, Entry { body, position }) {
            self.live_bytes -= previous.body.len() as u64 + 8;
        }
        if self.live_bytes <= CACHE_LIMIT {
            return;
        }
        // Leave space for several appends before sorting again.
        let mut oldest: Vec<_> = self
            .entries
            .iter()
            .map(|(key, entry)| (entry.position, key.clone()))
            .collect();
        oldest.sort_unstable();
        for (_, key) in oldest {
            if let Some(entry) = self.entries.remove(&key) {
                self.live_bytes -= entry.body.len() as u64 + 8;
            }
            if self.live_bytes <= CACHE_LIMIT * 3 / 4 {
                break;
            }
        }
    }
}

/// `[length u32][crc32 u32][JSON]`, little-endian.
fn write_frame(log: &mut File, body: &[u8]) -> std::io::Result<()> {
    log.write_all(&(body.len() as u32).to_le_bytes())?;
    log.write_all(&crc32fast::hash(body).to_le_bytes())?;
    log.write_all(body)
}

fn read_frame(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut length = [0; 4];
    let mut checksum = [0; 4];
    input.read_exact(&mut length).ok()?;
    input.read_exact(&mut checksum).ok()?;
    let length = u32::from_le_bytes(length) as usize;
    if length > FRAME_LIMIT {
        return None;
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body).ok()?;
    (crc32fast::hash(&body) == u32::from_le_bytes(checksum)).then_some(body)
}
