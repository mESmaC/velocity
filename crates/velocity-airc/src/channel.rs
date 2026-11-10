use crate::{Error, storage::Storage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct Entry {
    #[cfg_attr(feature = "bindings", specta(type = u32))]
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
    /// The PR an event is about, so readers such as the approval record (F7)
    /// never parse it back out of the display text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<u32>,
    /// The PR head commit the event applies to (`approved` binds to it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<crate::claims::Claim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency: Option<crate::waits::DependencyWait>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<crate::waits::Checkout>,
}

/// Event tags Velocity posts from what it observes (F14). Agents never type them.
pub const EVENT_TAGS: &[&str] = &[
    "claim",
    "refused",
    "overlap",
    "shares",
    "waits",
    "unblocked",
    "PR",
    "changes",
    "pushed",
    "approved",
    "merged",
    "order",
    "issue",
    "task",
    "failed",
    "discovery",
    "swarm",
    "left",
    "rebased",
    "updated",
    "conflict",
    "stalled",
    "at limit",
    "released",
    "broken",
    "reverted",
    "proved",
    "statement",
    "stress",
];

/// One event line: its type tag, the nick it is about, and what happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    tag: String,
    nick: String,
    text: String,
    pull_request: Option<u32>,
    head: Option<String>,
}

impl Event {
    pub fn new(tag: &str, nick: &str, text: &str) -> Self {
        Self {
            tag: tag.into(),
            nick: nick.into(),
            text: text.into(),
            pull_request: None,
            head: None,
        }
    }

    pub fn for_pull_request(mut self, number: u32, head: Option<&str>) -> Self {
        self.pull_request = Some(number);
        self.head = head.map(Into::into);
        self
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn nick(&self) -> &str {
        &self.nick
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn pull_request(&self) -> Option<u32> {
        self.pull_request
    }

    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Presence {
    pub member: String,
    pub nick: String,
    pub joined: bool,
}

pub struct Channel {
    pub(crate) directory: PathBuf,
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

    /// Your comments may broadcast; `nick:` resolves exactly and never silently broadcasts.
    pub fn comment(&self, text: &str) -> Result<Entry, Error> {
        require_text(text)?;
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        let mut record = entry(&entries, "message", "you", "you", text);
        if let Some((nickname, body)) = text.split_once(':') {
            let nickname = nickname.trim();
            if presence(&entries)
                .values()
                .any(|member| member.joined && member.nick == nickname)
            {
                require_text(body)?;
                record.to.push(nickname.into());
                record.text = body.trim().into();
            } else if !nickname.contains(char::is_whitespace) {
                return Err(Error::Invalid(format!("Unknown AIRC nick: {nickname}")));
            }
        }
        record.intent = Some("comment".into());
        storage.append(&record)?;
        Ok(record)
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

    /// Velocity records a Past work ledger entry (for example on every merge, F14)
    /// on behalf of `nick` without requiring `nick` to hold an active session.
    pub fn record_past_work(&self, nick_name: &str, text: &str) -> Result<Entry, Error> {
        require_text(nick_name)?;
        require_text(text)?;
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        let record = entry(
            &entries,
            "past_work",
            "velocity",
            &nick(nick_name, 16),
            text,
        );
        storage.append(&record)?;
        Ok(record)
    }

    /// Velocity posts events on behalf of the nick they name; posting needs no
    /// channel membership because the poster is Velocity itself.
    pub fn post_event(&self, event: Event) -> Result<Entry, Error> {
        if !EVENT_TAGS.contains(&event.tag.as_str()) {
            return Err(Error::Invalid(format!("Unknown event tag: {}", event.tag)));
        }
        require_text(&event.nick)?;
        require_text(&event.text)?;
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        let mut record = entry(&entries, &event.tag, "velocity", &event.nick, &event.text);
        record.pull_request = event.pull_request;
        record.head = event.head;
        storage.append(&record)?;
        Ok(record)
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

pub(crate) fn entry(entries: &[Entry], kind: &str, member: &str, nick: &str, text: &str) -> Entry {
    Entry {
        sequence: entries.last().map_or(1, |entry| entry.sequence + 1),
        time: chrono::Utc::now().to_rfc3339(),
        kind: kind.into(),
        member: member.into(),
        nick: nick.into(),
        text: text.into(),
        to: Vec::new(),
        intent: None,
        pull_request: None,
        head: None,
        claim: None,
        dependency: None,
        checkout: None,
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

pub(crate) fn nick(value: &str, limit: usize) -> String {
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

pub(crate) fn require_text(text: &str) -> Result<(), Error> {
    if text.trim().is_empty() {
        return Err(Error::Invalid("Text cannot be empty".into()));
    }
    Ok(())
}
