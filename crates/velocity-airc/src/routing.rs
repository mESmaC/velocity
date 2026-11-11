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

/// Only the event lines an agent must act on are pushed to it (#231). Its own events
/// (claims, PR, pushes, waits, rebases) are never sent back: it caused them, and the
/// tool call that did already returned the result. Routine lines about its PR from
/// others (PR duty's review claim and release, approvals) stay in the log for
/// `read_events`. `broken` follows [broken_for].
pub(crate) fn event_for(entry: &Entry, nick: &str, entries: &[Entry]) -> bool {
    if !entry.is_event() {
        return false;
    }
    if entry.to.iter().any(|recipient| recipient == nick) {
        return true;
    }
    match entry.kind.as_str() {
        // Both named agents must settle an overlap; the later one records a share or wait.
        "overlap" => names(entry, nick),
        // The holder learns another agent will change its file; the sharer already knows.
        "shares" => entry.nick != nick && names(entry, nick),
        "changes" | "conflict" | "updated" => {
            entry.nick != nick
                && owns_pull_request(entry, nick, entries, &own_claims(entries, nick))
        }
        "merged" => {
            let own = own_claims(entries, nick);
            if entry.nick == nick || owns_pull_request(entry, nick, entries, &own) {
                return false;
            }
            let claimed = own.iter().any(|claim| {
                claim.files.iter().any(|file| {
                    velocity_git::landing::contested_path(file) && entry.files.contains(file)
                })
            });
            claimed || edited_merge(entry, nick, entries)
        }
        _ => false,
    }
}

/// Generated overlap/share notices name whole nicks, never substring matches.
fn names(entry: &Entry, nick: &str) -> bool {
    entry
        .text
        .split(|character: char| {
            !character.is_alphanumeric() && character != '-' && character != '_'
        })
        .any(|word| word == nick)
}

fn own_claims(entries: &[Entry], nick: &str) -> Vec<crate::Claim> {
    crate::claims::claims(entries)
        .into_iter()
        .filter(|claim| claim.nick == nick)
        .collect()
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

/// Main-watcher notices name a broken merge commit. Check the actual checkout,
/// rather than notifying every agent on the project when only some contain it.
pub(crate) fn broken_for(entry: &Entry, nick: &str, entries: &[Entry]) -> bool {
    if entry.kind != "broken" {
        return false;
    }
    let Some(head) = entry.head.as_deref().filter(|head| {
        head.len() >= 7 && head.len() <= 64 && head.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) else {
        return false;
    };
    let Some(member) = entries
        .iter()
        .rev()
        .find(|record| record.kind == "join" && record.nick == nick)
        .map(|record| record.member.as_str())
    else {
        return false;
    };
    let Some(checkout) = entries
        .iter()
        .rev()
        .find(|record| record.member == member && record.checkout.is_some())
        .and_then(|record| record.checkout.as_ref())
    else {
        return false;
    };
    crate::process::hidden_command("git")
        .args([
            "-c",
            "core.longpaths=true",
            "-C",
            &checkout.worktree,
            "merge-base",
            "--is-ancestor",
            head,
            "HEAD",
        ])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn edited_merge(entry: &Entry, nick: &str, entries: &[Entry]) -> bool {
    let Some(head) = entry
        .head
        .as_deref()
        .filter(|head| head.len() >= 7 && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
    else {
        return false;
    };
    let Some(member) = entries
        .iter()
        .rev()
        .find(|record| record.kind == "join" && record.nick == nick)
        .map(|record| &record.member)
    else {
        return false;
    };
    let Some(checkout) = entries
        .iter()
        .rev()
        .find(|record| &record.member == member && record.checkout.is_some())
        .and_then(|record| record.checkout.as_ref())
    else {
        return false;
    };
    velocity_git::landing::edited_overlap(std::path::Path::new(&checkout.worktree), head)
        .is_ok_and(|files| !files.is_empty())
}
