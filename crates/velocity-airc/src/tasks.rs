//! Durable F8 requests. Only the native store owner writes state.json: agents queue adds,
//! updates and deletes here, and read the list from the snapshot that owner publishes.
use crate::{Channel, Entry, Error, channel::entry, storage::Storage};
use serde::{Deserialize, Serialize};

/// The request kinds the store owner drains, oldest first.
pub const TASK_REQUEST_KINDS: &[&str] =
    &["task_request", "task_update_request", "task_delete_request"];

/// The state an agent may move a task to. `now` belongs to the agent working on it, and
/// every task is a draft until the spec is locked, so neither can be requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum TaskTarget {
    Todo,
    Done,
}

/// The task an update or delete names, and the fields an update changes.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct TaskChange {
    pub task: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskTarget>,
}

impl TaskChange {
    pub fn new(task: u32) -> Self {
        Self {
            task,
            title: None,
            state: None,
        }
    }
}

/// One Task List row as `task_list` returns it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct TaskListRow {
    pub task: u32,
    pub title: String,
    /// draft, todo, now or done.
    pub state: String,
    /// The owning agent's live state, such as "agent working" or "#12 · in review".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
}

impl TaskListRow {
    pub fn new(task: u32, title: String, state: String) -> Self {
        Self {
            task,
            title,
            state,
            status: None,
            thread: None,
            added_by: None,
        }
    }
}

const TASK_LIST_FILE: &str = "tasks.json";

impl Channel {
    pub fn request_task(&self, member: &str, title: &str) -> Result<u64, Error> {
        let title = title.trim();
        if title.is_empty() {
            return Err(Error::Invalid("Task title must not be empty".into()));
        }
        self.queue_task_request(member, "task_request", title, None)
    }

    /// Queue a title or state change to one task (F8). At least one field must change.
    pub fn request_task_update(&self, member: &str, change: TaskChange) -> Result<u64, Error> {
        let title = change.title.as_deref().map(str::trim);
        if title == Some("") {
            return Err(Error::Invalid("Task title must not be empty".into()));
        }
        let mut parts = Vec::new();
        if let Some(title) = title {
            parts.push(format!("title \"{title}\""));
        }
        if let Some(state) = change.state {
            parts.push(format!("state {}", target_name(state)));
        }
        if parts.is_empty() {
            return Err(Error::Invalid(
                "Give a new title, a new state or both".into(),
            ));
        }
        let text = format!("update task #{}: {}", change.task, parts.join(", "));
        let change = TaskChange {
            title: title.map(str::to_owned),
            ..change
        };
        self.queue_task_request(member, "task_update_request", &text, Some(change))
    }

    /// Queue removing one task from the Task List (F8).
    pub fn request_task_delete(&self, member: &str, task: u32) -> Result<u64, Error> {
        let text = format!("delete task #{task}");
        self.queue_task_request(
            member,
            "task_delete_request",
            &text,
            Some(TaskChange::new(task)),
        )
    }

    fn queue_task_request(
        &self,
        member: &str,
        kind: &str,
        text: &str,
        change: Option<TaskChange>,
    ) -> Result<u64, Error> {
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        let presence = self.task_author_nick(member, &entries)?;
        let mut record = entry(&entries, kind, member, &presence, text);
        record.task_change = change;
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
                TASK_REQUEST_KINDS.contains(&request.kind.as_str())
                    && !entries.iter().any(|result| {
                        result.kind == "task_result"
                            && result.acknowledged == Some(request.sequence)
                    })
            })
            .cloned()
            .collect())
    }

    /// The store owner's answer to a queued request, once it has one.
    pub fn task_result(&self, request: u64) -> Result<Option<String>, Error> {
        Ok(self
            .read(request)?
            .into_iter()
            .find(|line| line.kind == "task_result" && line.acknowledged == Some(request))
            .map(|line| line.text))
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

    /// Save the Task List the store owner just read, for `task_list`. Writes only when the
    /// list changed, through a temporary file, so a reader never sees half a list.
    pub fn publish_task_list(&self, rows: &[TaskListRow]) -> Result<bool, Error> {
        let path = self.directory.join(TASK_LIST_FILE);
        let bytes = serde_json::to_vec_pretty(rows)?;
        if std::fs::read(&path).is_ok_and(|saved| saved == bytes) {
            return Ok(false);
        }
        let temporary = self.directory.join("tasks.json.tmp");
        std::fs::write(&temporary, &bytes)?;
        std::fs::rename(&temporary, &path)?;
        Ok(true)
    }

    /// The Task List as the store owner last published it; empty before its first pass.
    pub fn task_list(&self) -> Result<Vec<TaskListRow>, Error> {
        let path = self.directory.join(TASK_LIST_FILE);
        // The owner replaces the file by rename; Windows can refuse a read for that instant.
        for attempt in 0..5 {
            match std::fs::read(&path) {
                Ok(bytes) => return Ok(serde_json::from_slice(&bytes)?),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Vec::new());
                }
                Err(_) if attempt < 4 => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Vec::new())
    }
}

fn target_name(target: TaskTarget) -> &'static str {
    match target {
        TaskTarget::Todo => "todo",
        TaskTarget::Done => "done",
    }
}
