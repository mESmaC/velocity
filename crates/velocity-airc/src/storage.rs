use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;

use crate::{Channel, Entry, Error, remote::Remote};

pub(crate) struct Storage {
    directory: PathBuf,
    // The handle owns the cross-process lock for the whole read/modify/append operation.
    _lock: Option<File>,
    remote: Option<Remote>,
}

impl Storage {
    pub(crate) fn open(channel: &Channel) -> Result<Self, Error> {
        if let Some(remote) = &channel.remote {
            return Ok(Self {
                directory: PathBuf::new(),
                _lock: None,
                remote: Some(remote.clone()),
            });
        }
        let directory = &channel.directory;
        std::fs::create_dir_all(directory)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("channel.lock"))?;
        lock.lock()?;
        Ok(Self {
            directory: directory.into(),
            _lock: Some(lock),
            remote: None,
        })
    }

    pub(crate) fn read(&self) -> Result<Vec<Entry>, Error> {
        Ok(self.snapshot()?.as_ref().clone())
    }

    pub(crate) fn snapshot(&self) -> Result<Arc<Vec<Entry>>, Error> {
        if let Some(remote) = &self.remote {
            return remote.read(0).map(Arc::new);
        }
        // The cross-process lock is already held: offsets only observe committed,
        // stable files and mutations still allocate their sequence atomically.
        crate::tail::snapshot(&self.directory)
    }

    pub(crate) fn append(&self, entry: &Entry) -> Result<(), Error> {
        if let Some(remote) = &self.remote {
            return remote.append_record(entry).map(|_| ());
        }
        let date = chrono::Utc::now().format("%Y-%m-%d");
        let filename = match entry.kind.as_str() {
            "discovery" => "discoveries.jsonl".to_owned(),
            "past_work" => "past-work.jsonl".to_owned(),
            _ => format!("{date}.jsonl"),
        };
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join(filename))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        // A killed writer may leave an incomplete final record. Only newline-terminated
        // records are committed; remove that tail before the next append.
        let length = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        file.set_len(length as u64)?;
        file.seek(SeekFrom::End(0))?;
        let mut record = serde_json::to_vec(entry)?;
        record.push(b'\n');
        file.write_all(&record)?;
        file.sync_all()?;
        Ok(())
    }
}
