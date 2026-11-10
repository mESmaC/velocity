//! Durable F8 requests. Only the native store owner writes state.json.
use crate::{Channel, Entry, Error, channel::entry, storage::Storage};

impl Channel {
    pub fn request_task(&self, member: &str, title: &str) -> Result<u64, Error> {
        let title = title.trim();
        if title.is_empty() {
            return Err(Error::Invalid("Task title must not be empty".into()));
        }
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        let presence = self.task_author_nick(member, &entries)?;
        let record = entry(&entries, "task_request", member, &presence, title);
        storage.append(&record)?;
        Ok(record.sequence)
    }

    fn task_author_nick(&self, member: &str, entries: &[Entry]) -> Result<String, Error> {
        entries
            .iter()
            .rev()
            .find(|line| line.member == member && matches!(line.kind.as_str(), "join" | "leave"))
            .filter(|line| line.kind == "join")
            .map(|line| line.nick.clone())
            .ok_or_else(|| Error::Invalid("Join the channel first".into()))
    }

    pub fn pending_tasks(&self) -> Result<Vec<Entry>, Error> {
        let entries = self.read(0)?;
        Ok(entries
            .iter()
            .filter(|request| {
                request.kind == "task_request"
                    && !entries.iter().any(|result| {
                        result.kind == "task_result"
                            && result.acknowledged == Some(request.sequence)
                    })
            })
            .cloned()
            .collect())
    }

    pub fn finish_task(&self, request: &Entry, result: &str) -> Result<(), Error> {
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        if entries
            .iter()
            .any(|line| line.kind == "task_result" && line.acknowledged == Some(request.sequence))
        {
            return Ok(());
        }
        let mut record = entry(&entries, "task_result", "velocity", &request.nick, result);
        record.acknowledged = Some(request.sequence);
        storage.append(&record)
    }
}
