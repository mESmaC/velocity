//! Process-wide cursors survive the short-lived Channel handles used by app watchers.
use crate::{Entry, Error};
use std::collections::BTreeMap;
use std::fs::Metadata;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

#[derive(Clone, Default)]
struct LogTail {
    length: u64,
    committed: u64,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    entries: Vec<Entry>,
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
        let mut changed = self.logs.keys().any(|path| !paths.contains_key(path));
        // Stage changed logs: a malformed record must not advance any cursor.
        let mut updates = Vec::new();
        for (path, metadata) in &paths {
            let previous = self.logs.get(path);
            if previous.is_some_and(|log| log.unchanged(metadata)) {
                continue;
            }
            let mut log = previous.cloned().unwrap_or_default();
            log.refresh(path, metadata)?;
            updates.push((path.clone(), log));
            changed = true;
        }
        if changed {
            self.logs.retain(|path, _| paths.contains_key(path));
            self.logs.extend(updates);
            let mut entries: Vec<_> = self
                .logs
                .values()
                .flat_map(|log| log.entries.iter().cloned())
                .collect();
            entries.sort_by_key(|entry| entry.sequence);
            self.entries = Arc::new(entries);
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

    fn refresh(&mut self, path: &Path, metadata: &Metadata) -> Result<(), Error> {
        // Normal logs only append. Rebuild a replaced, truncated or rewritten file.
        if metadata.len() < self.length
            || self.created != metadata.created().ok()
            || (metadata.len() == self.length && self.modified != metadata.modified().ok())
        {
            *self = Self::default();
        }
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(self.committed))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let length = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        let appended = bytes[..length]
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice)
            .collect::<Result<Vec<Entry>, _>>()?;
        self.entries.extend(appended);
        self.committed += length as u64;
        self.length = metadata.len();
        self.created = metadata.created().ok();
        self.modified = metadata.modified().ok();
        Ok(())
    }
}
