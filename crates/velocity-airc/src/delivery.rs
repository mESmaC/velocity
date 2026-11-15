//! F14 targeted step-boundary delivery and held-message acknowledgement.
use crate::{Channel, Entry, Error};

impl Channel {
    /// A new thread starts at its own join, never at historical broadcast comments.
    /// Rejoining after restart keeps the original membership's lower bound.
    pub fn delivery_cursor(&self, member: &str, saved: u64) -> Result<u64, Error> {
        if saved != 0 {
            return Ok(saved);
        }
        Ok(self
            .snapshot()?
            .iter()
            .find(|entry| entry.kind == "join" && entry.member == member)
            .map_or(0, |entry| entry.sequence))
    }
}

/// Drain in sequence order. A pending or failed receiving turn retains the cursor,
/// so restart replays that message instead of losing queued input.
pub fn deliver_messages(
    channel: &Channel,
    nick: &str,
    after: u64,
    mut deliver: impl FnMut(&str, &str) -> Result<bool, String>,
) -> Result<u64, Error> {
    let mut cursor = after;
    for entry in channel.read(after)? {
        if let Some(message) = message_for(&entry, nick)
            && !deliver(&format!("airc:{}", entry.sequence), &message).map_err(Error::Invalid)?
        {
            break;
        }
        cursor = entry.sequence;
    }
    Ok(cursor)
}

/// Held messages retain the cursor, while durable per-line acknowledgements let
/// events and user comments pass them without being replayed after a restart.
pub fn deliver_targeted(
    channel: &Channel,
    nick: &str,
    after: u64,
    idle: bool,
    mut deliver: impl FnMut(&str, &str) -> Result<bool, String>,
) -> Result<u64, Error> {
    let mut cursor = after;
    let mut held = false;
    for (sequence, delivery) in pending(channel, nick, after, idle)? {
        match delivery {
            Pending::Held => held = true,
            Pending::Message(message) => {
                if !deliver(&format!("airc:{sequence}"), &message).map_err(Error::Invalid)? {
                    held = true;
                    continue;
                }
                channel.acknowledge(nick, sequence)?;
            }
            Pending::Skip => {}
        }
        if !held {
            cursor = sequence;
        }
    }
    Ok(cursor)
}

enum Pending {
    Skip,
    Held,
    Message(String),
}

fn pending(
    channel: &Channel,
    nick: &str,
    after: u64,
    idle: bool,
) -> Result<Vec<(u64, Pending)>, Error> {
    let tail = channel.read(after)?;
    if tail.is_empty() {
        return Ok(Vec::new());
    }
    // Routing needs modes, claims and checkouts from before the cursor. Borrow
    // the immutable cache; do not retain it across callbacks or acknowledgement
    // writes, which would force the tail cache to copy the entire history.
    let history = channel.snapshot()?;
    let mode = crate::mode_for(&history, nick);
    let acknowledged = history[history.partition_point(|entry| entry.sequence <= after)..]
        .iter()
        .filter(|entry| entry.nick == nick)
        .filter_map(|entry| entry.acknowledged)
        .collect::<std::collections::BTreeSet<_>>();
    Ok(tail
        .iter()
        .map(|entry| {
            let delivery = if acknowledged.contains(&entry.sequence) {
                Pending::Skip
            } else if let Some(message) = routed(entry, nick, &history, mode) {
                if hold(entry, nick, &history, mode, idle) {
                    Pending::Held
                } else {
                    Pending::Message(message)
                }
            } else {
                Pending::Skip
            };
            (entry.sequence, delivery)
        })
        .collect())
}

fn routed(
    entry: &Entry,
    nick: &str,
    entries: &[Entry],
    mode: crate::MessageMode,
) -> Option<String> {
    if crate::routing::event_for(entry, nick, entries)
        || crate::routing::broken_for(entry, nick, entries)
    {
        return Some(format!(
            "via AIRC · event {} · #{}\n{}",
            entry.kind,
            entry.sequence,
            if entry.kind == "merged" {
                format!(
                    "{}\nIf these files overlap your claim or edits, commit current edits and call airc_rebase now, then regenerate generated files and run the required checks. Velocity records the update outcome; resolve any conflicts in your own worktree. Otherwise update once at landing.",
                    entry.text
                )
            } else {
                entry.text.clone()
            }
        ));
    }
    if entry.kind != "message" {
        return None;
    }
    if entry.nick != "you" {
        let history =
            &entries[..entries.partition_point(|record| record.sequence < entry.sequence)];
        if mode == crate::MessageMode::Quarantined
            || crate::mode_for(entries, &entry.nick) == crate::MessageMode::Quarantined
            || crate::mode_for(history, &entry.nick) == crate::MessageMode::Quarantined
        {
            return None;
        }
    }
    message_for(entry, nick)
}

fn hold(
    entry: &Entry,
    nick: &str,
    entries: &[Entry],
    mode: crate::MessageMode,
    idle: bool,
) -> bool {
    mode == crate::MessageMode::HeadsDown
        && !idle
        && entry.kind == "message"
        && entry.nick != "you"
        && !entries.iter().any(|later| {
            later.sequence > entry.sequence
                && later.nick == nick
                && matches!(later.kind.as_str(), "PR" | "pushed" | "updated")
        })
}

/// Only named messages and your broadcast comments reach an agent. A sender never
/// receives its own line, and an inform never asks for an answer.
pub fn message_for(entry: &Entry, nick: &str) -> Option<String> {
    if entry.kind != "message" || entry.nick == nick {
        return None;
    }
    if !entry.to.iter().any(|recipient| recipient == nick)
        && !(entry.nick == "you" && entry.to.is_empty())
    {
        return None;
    }
    let instruction = match entry.intent.as_deref() {
        Some("inform") => "Information only; do not reply.",
        Some("question") => "Answer in AIRC.",
        Some("request") => "Reply in AIRC with done, or declined and the reason.",
        _ => "Information only; do not reply unless an answer was requested.",
    };
    Some(format!(
        "via AIRC · {} · #{}\n{}\n{instruction}",
        entry.nick, entry.sequence, entry.text
    ))
}
