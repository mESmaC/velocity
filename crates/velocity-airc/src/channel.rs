use crate::{Error, storage::Storage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub sequence: u64,
    pub time: String,
    pub kind: String,
    pub nick: String,
    pub member: String,
    pub text: String,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub intent: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Presence {
    pub member: String,
    pub nick: String,
    pub joined: bool,
}

pub struct Channel {
    directory: PathBuf,
}

impl Channel {
    /// Project identity, not display name, partitions channels even for same-name projects.
    pub fn open(app_data: &Path, project: &str) -> Result<Self, Error> {
        require_text(project)?;
        let key: String = project
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        std::fs::create_dir_all(app_data)?;
        let mut directory = std::fs::canonicalize(app_data)?.join("airc");
        for chunk in key.as_bytes().chunks(120) {
            directory.push(String::from_utf8_lossy(chunk).as_ref());
        }
        directory.push("channel");
        Storage::open(&directory)?;
        Ok(Self { directory })
    }

    pub(crate) fn connect(&self, member: &str) -> Result<std::fs::File, Error> {
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
        std::fs::create_dir_all(&directory)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("session.lock"))?;
        file.try_lock().map_err(|error| {
            Error::Invalid(format!(
                "Member already connected or session lock failed: {error}"
            ))
        })?;
        Ok(file)
    }
    pub fn read(&self, after: u64) -> Result<Vec<Entry>, Error> {
        Ok(Storage::open(&self.directory)?
            .read()?
            .into_iter()
            .filter(|entry| entry.sequence > after)
            .collect())
    }

    pub fn presence(&self) -> Result<Vec<Presence>, Error> {
        Ok(presence(&Storage::open(&self.directory)?.read()?)
            .into_values()
            .collect())
    }

    pub fn join(&self, member: &str, requested_nick: &str) -> Result<String, Error> {
        require_text(member)?;
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        let members = presence(&entries);
        if let Some(previous) = members.get(member).filter(|member| member.joined) {
            return Ok(previous.nick.clone());
        }
        let base = nick(requested_nick, 16);
        let mut candidate = base.clone();
        let mut suffix = 1_u64;
        while candidate == "you"
            || candidate == "velocity"
            || members
                .values()
                .any(|member| member.joined && member.nick == candidate)
        {
            let ending = format!("-{suffix}");
            candidate = format!(
                "{}{}",
                nick(&base, 16_usize.saturating_sub(ending.len())),
                ending
            );
            suffix += 1;
        }
        let entry = entry(&entries, "join", member, &candidate, "joined the channel");
        storage.append(&entry)?;
        Ok(candidate)
    }

    pub fn leave(&self, member: &str) -> Result<(), Error> {
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        if let Some(active) = presence(&entries)
            .get(member)
            .filter(|member| member.joined)
        {
            storage.append(&entry(
                &entries,
                "leave",
                member,
                &active.nick,
                "left the channel",
            ))?;
        }
        Ok(())
    }

    pub fn message(
        &self,
        member: &str,
        to: Vec<String>,
        intent: &str,
        text: &str,
    ) -> Result<(), Error> {
        if !matches!(intent, "inform" | "request" | "question") || to.is_empty() {
            return Err(Error::Invalid(
                "Messages require recipients and inform/request/question intent".into(),
            ));
        }
        self.post(member, "message", text, to, Some(intent.into()))
    }

    pub fn append_ledger(&self, member: &str, ledger: &str, text: &str) -> Result<(), Error> {
        if !matches!(ledger, "discovery" | "past_work") {
            return Err(Error::Invalid(
                "Ledger must be discovery or past_work".into(),
            ));
        }
        self.post(member, ledger, text, Vec::new(), None)
    }

    fn post(
        &self,
        member: &str,
        kind: &str,
        text: &str,
        to: Vec<String>,
        intent: Option<String>,
    ) -> Result<(), Error> {
        require_text(text)?;
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        let members = presence(&entries);
        let active = members
            .get(member)
            .filter(|member| member.joined)
            .ok_or_else(|| Error::Invalid("Join the channel first".into()))?;
        if to.iter().any(|recipient| {
            recipient != "you"
                && !members
                    .values()
                    .any(|member| member.joined && member.nick == *recipient)
        }) {
            return Err(Error::Invalid(
                "Message recipient is not in this channel".into(),
            ));
        }
        let mut record = entry(&entries, kind, member, &active.nick, text);
        record.to = to;
        record.intent = intent;
        storage.append(&record)
    }
}

fn entry(entries: &[Entry], kind: &str, member: &str, nick: &str, text: &str) -> Entry {
    Entry {
        sequence: entries.last().map_or(1, |entry| entry.sequence + 1),
        time: chrono::Utc::now().to_rfc3339(),
        kind: kind.into(),
        member: member.into(),
        nick: nick.into(),
        text: text.into(),
        to: Vec::new(),
        intent: None,
    }
}

fn presence(entries: &[Entry]) -> BTreeMap<String, Presence> {
    let mut members = BTreeMap::new();
    for entry in entries
        .iter()
        .filter(|entry| matches!(entry.kind.as_str(), "join" | "leave"))
    {
        members.insert(
            entry.member.clone(),
            Presence {
                member: entry.member.clone(),
                nick: entry.nick.clone(),
                joined: entry.kind == "join",
            },
        );
    }
    members
}

fn nick(value: &str, limit: usize) -> String {
    let leaf = value
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("agent");
    let words: Vec<_> = leaf
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let mut result = String::new();
    for word in words {
        let separator = if result.is_empty() { "" } else { "-" };
        if result.chars().count() + separator.len() + word.chars().count() > limit {
            break;
        }
        result.push_str(separator);
        result.push_str(word);
    }
    if result.is_empty() {
        result = leaf
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(limit)
            .collect();
    }
    if result.is_empty() {
        result = "agent".chars().take(limit).collect();
    }
    result
}

fn require_text(text: &str) -> Result<(), Error> {
    if text.trim().is_empty() {
        return Err(Error::Invalid("Text cannot be empty".into()));
    }
    Ok(())
}
