use crate::{Error, storage::Storage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct Entry {
    #[cfg_attr(feature = "bindings", specta(type = u32))]
    pub sequence: u64,
    /// Only stdio joins are leased to an OS session lock; native runner joins persist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<bool>,
    pub time: String,
    pub kind: String,
    pub nick: String,
    pub member: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Legacy change number carried by older channel records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "bindings", specta(skip))]
    pub pull_request: Option<u32>,
    /// The head snapshot the event applies to (`approved` binds to it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The Rosen change ID this event or workspace belongs to (R2, F14, #540).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<crate::claims::Claim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency: Option<crate::waits::DependencyWait>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<crate::waits::Checkout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_mode: Option<crate::MessageMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "bindings", specta(type = Option<u32>))]
    pub acknowledged: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// A discovery about Velocity itself rather than the project: the issue title its agent
    /// gave. Velocity files it on its own repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_issue: Option<String>,
    /// A discovery about Rosen itself, filed on mESmaC/rosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rosen_issue: Option<String>,
    /// The task and fields a `task_update_request` or `task_delete_request` names (F8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_change: Option<crate::TaskChange>,
    /// Structured event detail replacing raw JSON strings in `text` (#540).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<crate::EventPayload>,
}

impl Entry {
    /// The Rosen change ID recorded on this entry, falling back to checkout, claim or pre-#540 `head`.
    pub fn change_id(&self) -> Option<&str> {
        self.change
            .as_deref()
            .or_else(|| {
                self.checkout
                    .as_ref()
                    .and_then(|checkout| checkout.change.as_deref())
            })
            .or_else(|| {
                self.claim
                    .as_ref()
                    .and_then(|claim| claim.change.as_deref())
            })
            .or(self.head.as_deref())
    }
}

pub(crate) enum DiscoveryIssue {
    Velocity(String),
    Rosen(String),
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

/// Maximum length of a normalized AIRC nick (#532: long enough for `<short-slug>-<thread_id>`
/// without truncating distinguishing suffixes).
pub(crate) const NICK_LIMIT: usize = 48;

/// One event line: its type tag, the nick it is about, and what happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    tag: String,
    nick: String,
    text: String,
    pull_request: Option<u32>,
    head: Option<String>,
    change: Option<String>,
    recipients: Vec<String>,
    files: Vec<String>,
    pub(crate) task_request: Option<u64>,
}

impl Event {
    pub fn new(tag: &str, nick: &str, text: &str) -> Self {
        Self {
            tag: tag.into(),
            nick: nick.into(),
            text: text.into(),
            pull_request: None,
            head: None,
            change: None,
            recipients: Vec::new(),
            files: Vec::new(),
            task_request: None,
        }
    }

    pub fn for_pull_request(mut self, number: u32, head: Option<&str>) -> Self {
        self.pull_request = Some(number);
        self.head = head.map(Into::into);
        self
    }

    /// Attaches the Rosen change ID this event is about (#532).
    pub fn for_change(mut self, change: &str) -> Self {
        let change = change.trim();
        if !change.is_empty() {
            self.change = Some(change.to_owned());
        }
        self
    }

    /// The commit the event is about when no PR names it, such as a `broken` merge pushed
    /// straight to the base branch. Delivery routes `broken` events by this commit (F14).
    pub fn at_commit(mut self, commit: &str) -> Self {
        self.head = Some(commit.into());
        self
    }

    pub fn with_files(mut self, files: Vec<String>) -> Self {
        self.files = files;
        self
    }

    pub fn addressed_to(mut self, recipients: Vec<String>) -> Self {
        self.recipients = recipients;
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

    pub fn change(&self) -> Option<&str> {
        self.change.as_deref()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Presence {
    pub member: String,
    pub nick: String,
    pub joined: bool,
}

impl Presence {
    pub(crate) fn new(member: String, nick: String, joined: bool) -> Self {
        Self {
            member,
            nick,
            joined,
        }
    }
}

pub struct Channel {
    pub(crate) remote: Option<crate::remote::Remote>,
    pub(crate) directory: PathBuf,
}

impl Channel {
    /// Keep the launcher's wire IDs and local projections even if the tool process runs
    /// with a different machine environment or resolves the app data path differently.
    pub(crate) fn with_member_scope(mut self, scope: Option<String>) -> Result<Self, Error> {
        if let Some(scope) = scope {
            self.remote = self
                .remote
                .map(|remote| remote.with_member_scope(scope))
                .transpose()?;
        }
        Ok(self)
    }

    /// Rosen channels never fall back to a machine-local copy on disconnection.
    pub fn server_backed(&self) -> bool {
        self.remote.is_some()
    }

    /// The same server endpoint (a local daemon on a remote machine included) gives
    /// every host one channel authority. This never creates local channel files.
    pub fn at_endpoint(app_data: &Path, endpoint: &str) -> Self {
        Self {
            directory: PathBuf::new(),
            remote: Some(crate::remote::Remote::new(app_data, endpoint.into())),
        }
    }

    /// Project identity, not display name, partitions channels even for same-name projects.
    pub fn open(app_data: &Path, project: &str) -> Result<Self, Error> {
        Self::open_with_rosen(app_data, project, None)
    }

    /// Opens a project's channel using the store's Rosen configuration so non-Rosen projects
    /// never read `state.json` from disk (#555).
    pub fn open_for_project(app_data: &Path, project: &str, rosen: bool) -> Result<Self, Error> {
        Self::open_with_rosen(app_data, project, Some(rosen))
    }

    fn open_with_rosen(app_data: &Path, project: &str, rosen: Option<bool>) -> Result<Self, Error> {
        require_text(project)?;
        if let Some(remote) = crate::remote::Remote::for_project(app_data, project, rosen)? {
            return Ok(Self {
                directory: PathBuf::new(),
                remote: Some(remote),
            });
        }
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
        std::fs::create_dir_all(&directory)?;
        Ok(Self {
            directory,
            remote: None,
        })
    }

    pub fn read(&self, after: u64) -> Result<Vec<Entry>, Error> {
        if let Some(remote) = &self.remote {
            return remote.read(after);
        }
        let entries = self.snapshot()?;
        // Copy only the requested tail, after releasing the writer's lock.
        let start = entries.partition_point(|entry| entry.sequence <= after);
        Ok(entries[start..].to_vec())
    }

    /// Immutable routing context, shared with the log tail rather than cloned per agent.
    pub(crate) fn snapshot(&self) -> Result<std::sync::Arc<Vec<Entry>>, Error> {
        if let Some(remote) = &self.remote {
            return remote.snapshot();
        }
        Storage::open(self)?.snapshot()
    }

    pub fn tip(&self) -> Result<u64, Error> {
        if let Some(remote) = &self.remote {
            return remote.tip();
        }
        Ok(self.snapshot()?.last().map_or(0, |entry| entry.sequence))
    }

    /// The server database and watermark for Velocity's saved thread positions.
    pub fn server_event_head(&self) -> Result<Option<velocity_rosen::EventHead>, Error> {
        self.remote
            .as_ref()
            .map(|remote| remote.event_head())
            .transpose()
    }

    pub fn presence(&self) -> Result<Vec<Presence>, Error> {
        if let Some(remote) = &self.remote {
            return remote.presence();
        }
        Ok(presence(&self.read_presence_log(&Storage::open(self)?)?)
            .into_values()
            .collect())
    }

    pub fn join(&self, member: &str, requested_nick: &str) -> Result<String, Error> {
        self.join_with_session(member, requested_nick, false)
    }

    pub(crate) fn join_with_session(
        &self,
        member: &str,
        requested_nick: &str,
        session: bool,
    ) -> Result<String, Error> {
        if let Some(remote) = &self.remote {
            return remote.join(member, requested_nick);
        }
        require_text(member)?;
        let storage = Storage::open(self)?;
        let entries = self.read_presence_log(&storage)?;
        let members = presence(&entries);
        if let Some(previous) = members.get(member).filter(|member| member.joined) {
            if session {
                let mut record = entry(
                    &entries,
                    "join",
                    member,
                    &previous.nick,
                    "joined the channel",
                );
                record.session = Some(true);
                storage.append(&record)?;
            }
            return Ok(previous.nick.clone());
        }
        let candidate = available_nick(requested_nick, &members);
        let mut record = entry(&entries, "join", member, &candidate, "joined the channel");
        record.session = Some(session);
        storage.append(&record)?;
        Ok(candidate)
    }

    pub fn leave(&self, member: &str) -> Result<(), Error> {
        if let Some(remote) = &self.remote {
            return remote.leave(member);
        }
        let storage = Storage::open(self)?;
        let entries = self.read_presence_log(&storage)?;
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

    /// Exact nicks target one member; likely mistyped nicks never silently broadcast.
    pub fn comment(&self, text: &str) -> Result<Entry, Error> {
        if let Some(remote) = &self.remote {
            return remote.comment(text);
        }
        require_text(text)?;
        let storage = Storage::open(self)?;
        let entries = self.read_presence_log(&storage)?;
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
            } else if let Some(suggestion) = crate::comment::mistyped_nick(nickname, &entries) {
                return Err(Error::Invalid(format!(
                    "Unknown nick `{nickname}`. Did you mean `{suggestion}`?"
                )));
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
        if let Some(remote) = &self.remote {
            return remote.message(member, to, intent, text);
        }
        if !matches!(intent, "inform" | "request" | "question") || to.is_empty() {
            return Err(Error::Invalid(
                "Messages require recipients and inform/request/question intent".into(),
            ));
        }
        self.post(member, "message", text, to, Some(intent.into()))
    }

    pub fn append_ledger(&self, member: &str, ledger: &str, text: &str) -> Result<(), Error> {
        self.append_ledger_entry(member, ledger, text, None)
    }

    /// Records a ledger entry. A discovery with `velocity_issue` (a one-line issue title) is
    /// about Velocity itself, and Velocity files it on its own repository.
    pub fn append_ledger_entry(
        &self,
        member: &str,
        ledger: &str,
        text: &str,
        velocity_issue: Option<String>,
    ) -> Result<(), Error> {
        self.append_ledger_report(member, ledger, text, velocity_issue, None)
    }

    /// Reports a discovery to one of the tools' repositories, keeping project discoveries
    /// unmarked. The existing Velocity-only entry point remains compatible.
    pub fn append_ledger_report(
        &self,
        member: &str,
        ledger: &str,
        text: &str,
        velocity_issue: Option<String>,
        rosen_issue: Option<String>,
    ) -> Result<(), Error> {
        if !matches!(ledger, "discovery" | "past_work") {
            return Err(Error::Invalid(
                "Ledger must be discovery or past_work".into(),
            ));
        }
        let velocity_issue = velocity_issue.filter(|title| !title.trim().is_empty());
        let rosen_issue = rosen_issue.filter(|title| !title.trim().is_empty());
        if velocity_issue.is_some() && rosen_issue.is_some() {
            return Err(Error::Invalid(
                "Choose either velocity_issue or rosen_issue".into(),
            ));
        }
        let issue = match (velocity_issue, rosen_issue) {
            (Some(title), _) => Some(DiscoveryIssue::Velocity(title.trim().into())),
            (_, Some(title)) => Some(DiscoveryIssue::Rosen(title.trim().into())),
            _ => None,
        };
        if issue.is_some() && ledger != "discovery" {
            return Err(Error::Invalid(
                "Only a discovery can be reported as a Velocity or Rosen issue".into(),
            ));
        }
        let title = match &issue {
            Some(DiscoveryIssue::Velocity(title) | DiscoveryIssue::Rosen(title)) => Some(title),
            None => None,
        };
        if title.is_some_and(|title| title.contains(['\r', '\n']) || title.chars().count() > 200) {
            return Err(Error::Invalid(
                "The issue title must be one line of at most 200 characters".into(),
            ));
        }
        if let Some(remote) = &self.remote {
            return remote.ledger(member, ledger, text, issue);
        }
        self.post_with(member, ledger, text, Vec::new(), None, issue)
    }

    /// Velocity records a Past work ledger entry (for example on every merge, F14)
    /// on behalf of `nick` without requiring `nick` to hold an active session.
    pub fn record_past_work(&self, nick_name: &str, text: &str) -> Result<Entry, Error> {
        self.record_past_work_for_change(nick_name, None, text)
    }

    /// Velocity records a Past work ledger entry tied to an optional Rosen change ID (#532).
    pub fn record_past_work_for_change(
        &self,
        nick_name: &str,
        change: Option<&str>,
        text: &str,
    ) -> Result<Entry, Error> {
        require_text(nick_name)?;
        require_text(text)?;
        let storage = Storage::open(self)?;
        let entries = self.read_presence_log(&storage)?;
        let mut record = entry(
            &entries,
            "past_work",
            "velocity",
            &nick(nick_name, NICK_LIMIT),
            text,
        );
        record.change = change.filter(|id| !id.trim().is_empty()).map(str::to_owned);
        if let Some(remote) = &self.remote {
            return remote.append_record(&record);
        }
        storage.append(&record)?;
        Ok(record)
    }

    /// Velocity records a Discoveries ledger entry (for example a confirmed post-land
    /// failure and its cause, #540) without requiring a joined agent session.
    pub fn record_discovery(
        &self,
        nick_name: &str,
        change: Option<&str>,
        text: &str,
    ) -> Result<Entry, Error> {
        require_text(nick_name)?;
        require_text(text)?;
        let storage = Storage::open(self)?;
        let entries = self.read_presence_log(&storage)?;
        let mut record = entry(
            &entries,
            "discovery",
            "velocity",
            &nick(nick_name, NICK_LIMIT),
            text,
        );
        record.change = change.filter(|id| !id.trim().is_empty()).map(str::to_owned);
        if let Some(remote) = &self.remote {
            return remote.append_record(&record);
        }
        storage.append(&record)?;
        Ok(record)
    }

    /// Records a Velocity discovery only if no existing discovery entry contains `marker`.
    pub fn record_discovery_once(
        &self,
        nick_name: &str,
        change: Option<&str>,
        marker: &str,
        text: &str,
    ) -> Result<Option<Entry>, Error> {
        if self
            .ledger("discovery", 0)?
            .iter()
            .any(|existing| existing.text.contains(marker))
        {
            return Ok(None);
        }
        self.record_discovery(nick_name, change, text).map(Some)
    }

    /// Velocity posts events on behalf of the nick they name; posting needs no
    /// channel membership because the poster is Velocity itself.
    pub fn post_event(&self, event: Event) -> Result<Entry, Error> {
        if !EVENT_TAGS.contains(&event.tag.as_str()) {
            return Err(Error::Invalid(format!("Unknown event tag: {}", event.tag)));
        }
        require_text(&event.nick)?;
        require_text(&event.text)?;
        let storage = Storage::open(self)?;
        let entries = self.read_presence_log(&storage)?;
        let mut record = entry(&entries, &event.tag, "velocity", &event.nick, &event.text);
        record.pull_request = event.pull_request;
        record.head = event.head;
        record.change = event.change;
        record.to = event.recipients;
        record.files = event.files;
        record.acknowledged = event.task_request;
        if let Some(remote) = &self.remote {
            let result = remote.append_record(&record)?;
            if matches!(record.kind.as_str(), "PR" | "pushed" | "updated") {
                remote.release_held(&record.nick)?;
            }
            return Ok(result);
        }
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
        self.post_with(member, kind, text, to, intent, None)
    }

    fn post_with(
        &self,
        member: &str,
        kind: &str,
        text: &str,
        to: Vec<String>,
        intent: Option<String>,
        issue: Option<DiscoveryIssue>,
    ) -> Result<(), Error> {
        require_text(text)?;
        let storage = Storage::open(self)?;
        let entries = self.read_presence_log(&storage)?;
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
        match issue {
            Some(DiscoveryIssue::Velocity(title)) => record.velocity_issue = Some(title),
            Some(DiscoveryIssue::Rosen(title)) => record.rosen_issue = Some(title),
            None => {}
        }
        storage.append(&record)
    }
}

/// Uniform UTC RFC 3339 timestamp with millisecond precision (`Z` suffix) (#540).
pub(crate) fn format_utc_millis(time: chrono::DateTime<chrono::Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(crate) fn format_timestamp_millis(milliseconds: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(milliseconds).map(format_utc_millis)
}

pub(crate) fn parse_timestamp_millis(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|time| time.timestamp_millis())
}

pub(crate) fn entry(entries: &[Entry], kind: &str, member: &str, nick: &str, text: &str) -> Entry {
    let now = chrono::Utc::now().timestamp_millis();
    // Keep local sequence timestamps monotonic even across backward clock steps.
    let floor = entries
        .last()
        .and_then(|previous| parse_timestamp_millis(&previous.time))
        .unwrap_or(now);
    let stamp = format_timestamp_millis(now.max(floor))
        .unwrap_or_else(|| format_utc_millis(chrono::Utc::now()));
    Entry {
        session: None,
        sequence: entries.last().map_or(1, |entry| entry.sequence + 1),
        time: stamp,
        kind: kind.into(),
        member: member.into(),
        nick: nick.into(),
        text: text.into(),
        to: Vec::new(),
        intent: None,
        pull_request: None,
        head: None,
        change: None,
        claim: None,
        dependency: None,
        checkout: None,
        message_mode: None,
        acknowledged: None,
        files: Vec::new(),
        velocity_issue: None,
        rosen_issue: None,
        task_change: None,
        payload: None,
    }
}

pub(crate) fn presence(entries: &[Entry]) -> BTreeMap<String, Presence> {
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
    // Preserve a trailing numeric suffix (such as a thread ID or `-2`) when shortening
    // a long name so distinct threads never collapse onto the same prefix (#532).
    let joined_len: usize = words.iter().map(|word| word.chars().count()).sum::<usize>()
        + words.len().saturating_sub(1);
    let suffix = (joined_len > limit && words.len() > 1)
        .then(|| words.last().copied())
        .flatten()
        .filter(|word| word.chars().all(|character| character.is_ascii_digit()))
        .filter(|word| word.len() + 2 <= limit);
    let (body_words, body_limit) = match suffix {
        Some(digits) => (
            &words[..words.len() - 1],
            limit.saturating_sub(digits.len() + 1),
        ),
        None => (&words[..], limit),
    };
    let mut result = join_words(body_words, body_limit);
    if result.is_empty() {
        result = leaf
            .chars()
            .filter(|character| character.is_alphanumeric())
            .take(body_limit)
            .collect();
    }
    if result.is_empty() {
        result = "agent".chars().take(body_limit).collect();
    }
    if let Some(digits) = suffix {
        result.push('-');
        result.push_str(digits);
    }
    result
}

fn join_words(words: &[&str], limit: usize) -> String {
    let mut result = String::new();
    for word in words {
        let separator = if result.is_empty() { "" } else { "-" };
        if result.chars().count() + separator.len() + word.chars().count() > limit {
            break;
        }
        result.push_str(separator);
        result.push_str(word);
    }
    result
}

pub(crate) fn require_text(text: &str) -> Result<(), Error> {
    if text.trim().is_empty() {
        return Err(Error::Invalid("Text cannot be empty".into()));
    }
    Ok(())
}

fn available_nick(requested: &str, members: &BTreeMap<String, Presence>) -> String {
    let base = nick(requested, NICK_LIMIT);
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
            nick(&base, NICK_LIMIT.saturating_sub(ending.len())),
            ending
        );
        suffix += 1;
    }
    candidate
}
