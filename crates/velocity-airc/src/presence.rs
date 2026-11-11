use std::fs::{File, OpenOptions, TryLockError};
use std::io::ErrorKind;

use crate::{Channel, Entry, Error, channel::entry, channel::require_text, storage::Storage};

impl Channel {
    fn session_path(&self, member: &str) -> Result<std::path::PathBuf, Error> {
        require_text(member)?;
        let key: String = member
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let mut directory = self.directory.join("sessions");
        for chunk in key.as_bytes().chunks(120) {
            directory.push(String::from_utf8_lossy(chunk).as_ref());
        }
        Ok(directory.join("session.lock"))
    }

    fn session_file(&self, member: &str) -> Result<File, Error> {
        let path = self.session_path(member)?;
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?)
    }

    pub(crate) fn connect(&self, member: &str) -> Result<File, Error> {
        // Serialize reconnect and reconciliation so an old join cannot borrow the new
        // connection's lock and keep a stale nick alive.
        let storage = Storage::open(&self.directory)?;
        let legacy_session = self.session_path(member)?.exists();
        let connection = self.session_file(member)?;
        connection.try_lock().map_err(|error| {
            Error::Invalid(format!(
                "Member already connected or session lock failed: {error}"
            ))
        })?;
        let mut entries = storage.read()?;
        if let Some(previous) = entries
            .iter()
            .rev()
            .find(|record| {
                record.member == member && matches!(record.kind.as_str(), "join" | "leave")
            })
            .filter(|record| record.kind == "join" && record.session.unwrap_or(legacy_session))
            .cloned()
        {
            append_disconnect(&storage, &mut entries, &previous)?;
        }
        Ok(connection)
    }

    pub(crate) fn read_presence_log(&self, storage: &Storage) -> Result<Vec<Entry>, Error> {
        let mut entries = storage.read()?;
        let mut latest = std::collections::BTreeMap::new();
        for record in &entries {
            if matches!(record.kind.as_str(), "join" | "leave") {
                latest.insert(record.member.clone(), record.clone());
            }
        }
        for record in latest
            .values()
            .filter(|record| record.kind == "join" && record.session != Some(false))
        {
            // Before session tags existed, only stdio members created session.lock.
            // Native joins without that file must keep their historical membership.
            let connection = match OpenOptions::new()
                .write(true)
                .open(self.session_path(&record.member)?)
            {
                Ok(connection) => connection,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    if record.session == Some(true) {
                        append_disconnect(storage, &mut entries, record)?;
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match connection.try_lock() {
                Ok(()) => append_disconnect(storage, &mut entries, record)?,
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) if error.kind() == ErrorKind::WouldBlock => {}
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        Ok(entries)
    }
}

fn append_disconnect(
    storage: &Storage,
    entries: &mut Vec<Entry>,
    previous: &Entry,
) -> Result<(), Error> {
    let record = entry(
        entries,
        "leave",
        &previous.member,
        &previous.nick,
        "session disconnected",
    );
    storage.append(&record)?;
    entries.push(record);
    Ok(())
}
