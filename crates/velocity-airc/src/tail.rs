//! Process-wide cursors survive the short-lived Channel handles used by app watchers.
use crate::{Entry, Error};
use std::collections::BTreeMap;
use std::fs::Metadata;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

#[derive(Default)]
struct LogTail {
    length: u64,
    committed: u64,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
}

#[derive(Default)]
struct ChannelTail {
    logs: BTreeMap<PathBuf, LogTail>,
    entries: Arc<Vec<Entry>>,
}

type Cache = Mutex<BTreeMap<PathBuf, Arc<Mutex<ChannelTail>>>>;
static CHANNELS: OnceLock<Cache> = OnceLock::new();

/// Caller holds channel.lock, always before acquiring the process-local cache lock.
pub(crate) fn snapshot(directory: &Path) -> Result<Arc<Vec<Entry>>, Error> {
    let cached = CHANNELS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| Error::Invalid("AIRC cache lock poisoned".into()))?
        .entry(directory.to_owned())
        .or_default()
        .clone();
    let mut cached = cached
        .lock()
        .map_err(|_| Error::Invalid("AIRC channel cache lock poisoned".into()))?;
    cached.refresh(directory)
}

impl ChannelTail {
    fn refresh(&mut self, directory: &Path) -> Result<Arc<Vec<Entry>>, Error> {
        let mut paths = BTreeMap::new();
        for item in std::fs::read_dir(directory)? {
            let item = item?;
            if item
                .path()
                .extension()
                .is_some_and(|value| value == "jsonl")
            {
                paths.insert(item.path(), item.metadata()?);
            }
        }
        let rebuild = self.logs.iter().any(|(path, log)| {
            paths
                .get(path)
                .is_none_or(|metadata| log.replaced(metadata))
        });
        if rebuild {
            // Stage recovery too: malformed replacements must not publish a partial view.
            let mut replacement = Self::default();
            let entries = replacement.update(&paths)?;
            *self = replacement;
            return Ok(entries);
        }
        self.update(&paths)
    }

    fn update(&mut self, paths: &BTreeMap<PathBuf, Metadata>) -> Result<Arc<Vec<Entry>>, Error> {
        let mut updates = Vec::new();
        let mut appended = Vec::new();
        for (path, metadata) in paths {
            let previous = self.logs.get(path);
            if previous.is_some_and(|log| log.unchanged(metadata)) {
                continue;
            }
            let (log, entries) = previous
                .unwrap_or(&LogTail::default())
                .read(path, metadata)?;
            updates.push((path.clone(), log));
            appended.extend(entries);
        }
        // No cursor advances unless every changed file parsed successfully.
        self.logs.extend(updates);
        if !appended.is_empty() {
            appended.sort_by_key(|entry| entry.sequence);
            let out_of_order = self
                .entries
                .last()
                .zip(appended.first())
                .is_some_and(|(last, first)| last.sequence >= first.sequence);
            let entries = Arc::make_mut(&mut self.entries);
            entries.extend(appended);
            if out_of_order {
                entries.sort_by_key(|entry| entry.sequence);
            }
        }
        Ok(self.entries.clone())
    }
}

impl LogTail {
    fn unchanged(&self, metadata: &Metadata) -> bool {
        self.length == metadata.len()
            && self.created == metadata.created().ok()
            && self.modified == metadata.modified().ok()
    }

    fn replaced(&self, metadata: &Metadata) -> bool {
        metadata.len() < self.length
            || self.created != metadata.created().ok()
            || (metadata.len() == self.length && self.modified != metadata.modified().ok())
    }

    fn read(&self, path: &Path, metadata: &Metadata) -> Result<(Self, Vec<Entry>), Error> {
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(self.committed))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let length = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        let entries = bytes[..length]
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice)
            .collect::<Result<Vec<Entry>, _>>()?;
        let log = Self {
            committed: self.committed + length as u64,
            length: metadata.len(),
            created: metadata.created().ok(),
            modified: metadata.modified().ok(),
        };
        Ok((log, entries))
    }
}
