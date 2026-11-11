//! F14 event log posted by Velocity from claims, GitHub and the Task List, with
//! agent messages and ledgers kept separate.
use crate::channel::{EVENT_TAGS, nick, require_text};
use crate::{Channel, Entry, Error, Event};

impl Entry {
    /// True when this entry belongs to the F14 Events log (and not to agent
    /// messages or the Past work ledger).
    pub fn is_event(&self) -> bool {
        EVENT_TAGS.contains(&self.kind.as_str())
    }

    /// True when this entry is an agent-to-agent message or a user comment.
    pub fn is_message(&self) -> bool {
        self.kind == "message"
    }

    /// When a `discovery` ledger entry is viewed in the Events log, its text is
    /// prefixed with `logged a discovery · ` (SPEC.md F14, reference UI).
    pub fn as_event_entry(&self) -> Self {
        let mut copy = self.clone();
        if copy.kind == "discovery"
            && copy.member != "velocity"
            && !copy.text.starts_with("logged a discovery · ")
        {
            copy.text = format!("logged a discovery · {}", copy.text);
        }
        copy
    }
}

impl Channel {
    /// Reads only event log entries after `after`, keeping agent messages and
    /// Past work ledger entries separate (F14).
    pub fn events(&self, after: u64) -> Result<Vec<Entry>, Error> {
        Ok(self
            .read(after)?
            .into_iter()
            .filter(Entry::is_event)
            .map(|entry| entry.as_event_entry())
            .collect())
    }

    /// Reads only agent coordination messages and user comments after `after`,
    /// keeping Velocity's event log and ledgers separate (F14).
    pub fn messages(&self, after: u64) -> Result<Vec<Entry>, Error> {
        Ok(self
            .read(after)?
            .into_iter()
            .filter(Entry::is_message)
            .collect())
    }

    /// Reads either the `discovery` or `past_work` ledger after `after` (F14).
    pub fn ledger(&self, name: &str, after: u64) -> Result<Vec<Entry>, Error> {
        if !matches!(name, "discovery" | "past_work") {
            return Err(Error::Invalid(
                "Ledger must be discovery or past_work".into(),
            ));
        }
        Ok(self
            .read(after)?
            .into_iter()
            .filter(|entry| entry.kind == name)
            .collect())
    }

    /// Posts `event` only if no identical `(tag, nick, pull_request, head, text)`
    /// event is already recorded in the channel. Returns `Some(entry)` when a
    /// new event was appended, or `None` when it was already recorded.
    pub fn post_event_once(&self, event: Event) -> Result<Option<Entry>, Error> {
        let existing = self.read(0)?;
        if existing.iter().any(|entry| matches_event(entry, &event)) {
            return Ok(None);
        }
        self.post_event(event).map(Some)
    }

    /// Records a merged PR in both the event log (`merged`) and the Past work
    /// ledger (`<title>: merged #<n>`, SPEC.md F14: "Every merge produces a
    /// `merged` event and a Past work entry"), deduplicated by PR number.
    pub fn post_pull_request_merged(
        &self,
        owner_nick: &str,
        number: u32,
        title: &str,
        head: &str,
    ) -> Result<Option<Entry>, Error> {
        self.post_pull_request_merged_with_files(owner_nick, number, title, head, Vec::new())
    }

    pub fn post_pull_request_merged_with_files(
        &self,
        owner_nick: &str,
        number: u32,
        title: &str,
        head: &str,
        files: Vec<String>,
    ) -> Result<Option<Entry>, Error> {
        let event = Event::pull_request_merged(owner_nick, number, title, head)?.with_files(files);
        let existing = self.read(0)?;
        let already_merged = existing
            .iter()
            .any(|entry| entry.kind == "merged" && entry.pull_request == Some(number));
        if already_merged {
            return Ok(None);
        }
        let merged = self.post_event(event)?;
        let ledger_line = format!("{} · merged #{number}", one_line(title));
        if !existing
            .iter()
            .any(|entry| entry.kind == "past_work" && entry.text == ledger_line)
        {
            self.record_past_work(&merged.nick, &ledger_line)?;
        }
        Ok(Some(merged))
    }
}

impl Event {
    /// `claim`: `<nick> claimed <task> · <files>` (F14).
    pub fn claimed(owner_nick: &str, task: &str, files: &[&str]) -> Result<Self, Error> {
        require_text(task)?;
        let actor = nick(owner_nick, 16);
        let suffix = if files.is_empty() {
            String::new()
        } else {
            format!(" · {}", files.join(", "))
        };
        Ok(Self::new(
            "claim",
            &actor,
            &format!("claimed {}{suffix}", one_line(task)),
        ))
    }

    /// `PR`: `<nick> opened #<n> <title>` (F6, F14).
    pub fn pull_request_opened(
        owner_nick: &str,
        number: u32,
        title: &str,
        head: &str,
    ) -> Result<Self, Error> {
        require_text(title)?;
        require_text(head)?;
        let actor = nick(owner_nick, 16);
        Ok(Self::new(
            "PR",
            &actor,
            &format!("opened #{number} {}", one_line(title)),
        )
        .for_pull_request(number, Some(head)))
    }

    /// `pushed`: `<nick> pushed <sha7> to #<n>` (F6, F14).
    pub fn pull_request_pushed(owner_nick: &str, number: u32, head: &str) -> Result<Self, Error> {
        require_text(head)?;
        let actor = nick(owner_nick, 16);
        Ok(Self::new(
            "pushed",
            &actor,
            &format!("pushed {} to #{number}", short_sha(head)),
        )
        .for_pull_request(number, Some(head)))
    }

    /// `merged`: `<nick> #<n> <title> merged · worktree deleted` (F6, F7, F14).
    pub fn pull_request_merged(
        owner_nick: &str,
        number: u32,
        title: &str,
        head: &str,
    ) -> Result<Self, Error> {
        require_text(title)?;
        require_text(head)?;
        let actor = nick(owner_nick, 16);
        Ok(Self::new(
            "merged",
            &actor,
            &format!("#{number} {} merged · worktree deleted", one_line(title)),
        )
        .for_pull_request(number, Some(head)))
    }

    /// `issue`: `<scout> filed #<n> <title>` (F7, F9, F14).
    pub fn issue_filed(actor_nick: &str, number: u32, title: &str) -> Result<Self, Error> {
        require_text(title)?;
        let actor = nick(actor_nick, 16);
        Ok(Self::new(
            "issue",
            &actor,
            &format!("filed #{number} {}", one_line(title)),
        ))
    }

    /// `task`: `<actor> added to the Task List: <title>` (F3, F8, F14).
    pub fn task_added(actor_nick: &str, title: &str) -> Result<Self, Error> {
        require_text(title)?;
        let actor = if actor_nick == "you" {
            String::from("you")
        } else {
            nick(actor_nick, 16)
        };
        Ok(Self::new(
            "task",
            &actor,
            &format!("added to the Task List: {}", one_line(title)),
        ))
    }

    /// Saved task identity keeps replay idempotent without hiding distinct tasks
    /// with the same title. Identifiers are scoped to this project's channel.
    pub fn task_saved(actor_nick: &str, identifier: u32, title: &str) -> Result<Self, Error> {
        let event = Self::task_added(actor_nick, title)?;
        Ok(Self::new(
            event.tag(),
            event.nick(),
            &format!("{} · task #{identifier}", event.text()),
        ))
    }

    /// `failed`: `<nick> failed · <reason> · its claim is held` (§2.1, F14).
    pub fn thread_failed(owner_nick: &str, reason: &str) -> Result<Self, Error> {
        require_text(reason)?;
        let actor = nick(owner_nick, 16);
        Ok(Self::new(
            "failed",
            &actor,
            &format!("failed · {} · its claim is held", one_line(reason)),
        ))
    }

    /// swarm: you started a goal swarm · <n> agents · one prompt: "<prompt>" (F14, F16).
    pub fn goal_swarm_started(agents: usize, prompt: &str) -> Result<Self, Error> {
        require_text(prompt)?;
        Ok(Self::new(
            "swarm",
            "you",
            &format!(
                "started a goal swarm · {agents} agent{} · one prompt: \"{}\"",
                if agents == 1 { "" } else { "s" },
                one_line(prompt)
            ),
        ))
    }

    /// left: <nick> left · <reason> when a goal swarm agent leaves with nothing unclaimed (F16).
    pub fn agent_left(owner_nick: &str, reason: &str) -> Result<Self, Error> {
        require_text(reason)?;
        let actor = nick(owner_nick, 16);
        Ok(Self::new(
            "left",
            &actor,
            &format!("left · {}", one_line(reason)),
        ))
    }
}

fn matches_event(entry: &Entry, event: &Event) -> bool {
    entry.kind == event.tag()
        && entry.nick == event.nick()
        && entry.pull_request == event.pull_request()
        && entry.head.as_deref() == event.head()
        && (entry.pull_request.is_some() || entry.text == event.text())
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn one_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}
