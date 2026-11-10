//! Durable message modes share the channel lock with claim updates.
use crate::{Channel, Entry, Error, channel::entry, storage::Storage};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum MessageMode {
    #[default]
    Open,
    HeadsDown,
    Quarantined,
}

pub fn mode_for(entries: &[Entry], nick: &str) -> MessageMode {
    entries
        .iter()
        .rev()
        .find(|entry| entry.nick == nick && entry.message_mode.is_some())
        .and_then(|entry| entry.message_mode)
        .unwrap_or_default()
}

pub(crate) fn require_unfrozen(entries: &[Entry], member: &str) -> Result<(), Error> {
    let nick = entries
        .iter()
        .rev()
        .find(|entry| entry.kind == "join" && entry.member == member)
        .map(|entry| entry.nick.as_str())
        .unwrap_or(member);
    if mode_for(entries, nick) == MessageMode::Quarantined {
        return Err(Error::Invalid("Claims are frozen while quarantined".into()));
    }
    Ok(())
}

impl Channel {
    pub fn set_message_mode(&self, nick: &str, mode: MessageMode) -> Result<(), Error> {
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        let member = self.presence_from_entries(&entries, nick)?;
        let mut record = entry(
            &entries,
            "message_mode",
            &member,
            nick,
            "Message mode changed by you",
        );
        record.message_mode = Some(mode);
        storage.append(&record)
    }

    fn presence_from_entries(&self, entries: &[Entry], nick: &str) -> Result<String, Error> {
        entries
            .iter()
            .rev()
            .find(|entry| entry.nick == nick && entry.kind == "join")
            .map(|entry| entry.member.clone())
            .ok_or_else(|| Error::Invalid("Unknown AIRC nick".into()))
    }

    pub(crate) fn acknowledge(&self, nick: &str, sequence: u64) -> Result<(), Error> {
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        let mut record = entry(
            &entries,
            "delivery_ack",
            "velocity",
            nick,
            "Delivered at step boundary",
        );
        record.acknowledged = Some(sequence);
        storage.append(&record)
    }
}

/// Explicit recipients win; status notices also follow their named claim and waits.
pub(crate) fn event_for(entry: &Entry, nick: &str, entries: &[Entry]) -> bool {
    if !entry.is_event() {
        return false;
    }
    if entry.to.iter().any(|recipient| recipient == nick) || entry.nick == nick {
        return true;
    }
    if entry
        .dependency
        .as_ref()
        .is_some_and(|wait| wait.nick == nick || wait.holder_nick == nick)
    {
        return true;
    }
    // Generated overlap/share notices name whole nicks, never substring matches.
    if matches!(entry.kind.as_str(), "overlap" | "shares" | "refused") {
        return entry
            .text
            .split(|character: char| {
                !character.is_alphanumeric() && character != '-' && character != '_'
            })
            .any(|word| word == nick);
    }
    let own = crate::claims::claims(entries)
        .into_iter()
        .filter(|claim| claim.nick == nick)
        .collect::<Vec<_>>();
    if owns_pull_request(entry, nick, entries, &own) {
        return true;
    }
    if entry.kind == "merged" {
        return own
            .iter()
            .any(|claim| claim.files.iter().any(|file| entry.files.contains(file)));
    }
    false
}

fn owns_pull_request(
    entry: &Entry,
    nick: &str,
    entries: &[Entry],
    claims: &[crate::Claim],
) -> bool {
    entry.pull_request.is_some_and(|number| {
        claims
            .iter()
            .any(|claim| claim.key() == format!("pr:{number}"))
            || entries.iter().any(|record| {
                matches!(record.kind.as_str(), "PR" | "claim")
                    && record.pull_request == Some(number)
                    && record.nick == nick
            })
    })
}
