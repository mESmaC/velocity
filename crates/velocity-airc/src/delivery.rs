//! F14 message selection. Events remain on demand until targeted event delivery ships.
use crate::{Channel, Entry, Error};

impl Channel {
    /// A new thread starts at its own join, never at historical broadcast comments.
    /// Rejoining after restart keeps the original membership's lower bound.
    pub fn delivery_cursor(&self, member: &str, saved: u64) -> Result<u64, Error> {
        if saved != 0 {
            return Ok(saved);
        }
        Ok(self
            .read(0)?
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
        if let Some(message) = message_for(&entry, nick) {
            let identifier = format!("airc:{}", entry.sequence);
            if !deliver(&identifier, &message).map_err(Error::Invalid)? {
                break;
            }
        }
        cursor = entry.sequence;
    }
    Ok(cursor)
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
        Some("question") => "Answer in AIRC.",
        Some("request") => "Reply in AIRC with done, or declined and the reason.",
        _ => "Information only; do not reply unless an answer was requested.",
    };
    Some(format!(
        "via AIRC · {} · #{}\n{}\n{instruction}",
        entry.nick, entry.sequence, entry.text
    ))
}
