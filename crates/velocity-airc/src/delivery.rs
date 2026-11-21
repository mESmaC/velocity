//! F14 targeted step-boundary delivery and held-message acknowledgement.
use std::collections::BTreeMap;

use crate::{Channel, Entry, Error};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryPriority {
    Information,
    NeedsAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub sequence: u64,
    pub message: String,
    pub priority: DeliveryPriority,
    server_identity: Option<String>,
    overlaps: Vec<Overlap>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Overlap {
    agents: [String; 2],
    paths: Vec<String>,
}

impl Delivery {
    pub fn with_server_identity(mut self, identity: Option<&str>) -> Self {
        self.server_identity = identity.map(str::to_owned);
        self
    }

    pub fn identifier(&self) -> String {
        match &self.server_identity {
            Some(identity) => format!("airc:{identity}:{}", self.sequence),
            None => format!("airc:{}", self.sequence),
        }
    }
}

/// Render once at the prompt boundary, including overlaps gathered on earlier passes.
pub fn prompt_lines(deliveries: &[Delivery]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut pairs = BTreeMap::new();
    for delivery in deliveries {
        if delivery.overlaps.is_empty() {
            lines.push(delivery.message.clone());
        }
        for overlap in &delivery.overlaps {
            let mut pair = overlap.agents.clone();
            pair.sort();
            let (index, paths) = pairs.entry(pair).or_insert_with(|| {
                lines.push(String::new());
                (lines.len() - 1, Vec::new())
            });
            for path in &overlap.paths {
                if !paths.contains(path) {
                    paths.push(path.clone());
                }
            }
            lines[*index] = format!(
                "via AIRC · {} overlaps {} on {}; record a share or wait before editing",
                overlap.agents[0],
                overlap.agents[1],
                paths.join(", ")
            );
        }
    }
    lines
}

impl Channel {
    /// A new thread starts at its own join, never at historical broadcast comments.
    /// Rejoining after restart keeps the original membership's lower bound.
    pub fn delivery_cursor(&self, member: &str, saved: u64) -> Result<u64, Error> {
        if let Some(remote) = &self.remote {
            return remote.delivery_cursor(member, saved);
        }
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
    mut deliver: impl FnMut(&Delivery) -> Result<bool, String>,
) -> Result<u64, Error> {
    if channel.server_backed() {
        return crate::remote_delivery::deliver(channel, nick, after, idle, deliver);
    }
    let mut cursor = after;
    let mut held = false;
    for (sequence, delivery) in pending(channel, nick, after, idle)? {
        match delivery {
            Pending::Held => held = true,
            Pending::Message(message) => {
                if !deliver(&message).map_err(Error::Invalid)? {
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
    Message(Delivery),
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
            } else if let Some(message) = routed(entry, nick, &history, mode, false) {
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

pub(crate) fn routed(
    entry: &Entry,
    nick: &str,
    entries: &[Entry],
    mode: crate::MessageMode,
    native_land: bool,
) -> Option<Delivery> {
    if crate::routing::event_for(entry, nick, entries)
        || crate::routing::broken_for(entry, nick, entries)
    {
        let needs_rebase = entry.kind == "merged"
            && !native_land
            && crate::routing::merge_touches(entry, nick, entries);
        let message = format!(
            "via AIRC · event {} · #{}\n{}",
            entry.kind,
            entry.sequence,
            if needs_rebase {
                format!(
                    "{}\nIf these files overlap your claim or edits, commit current edits and call airc_rebase now, then regenerate generated files and run the required checks. Velocity records the update outcome; resolve any conflicts in your own worktree. Otherwise update once at landing.",
                    entry.text
                )
            } else {
                entry.text.clone()
            }
        );
        let priority = match entry.kind.as_str() {
            "overlap" | "shares" => DeliveryPriority::Information,
            "merged" if !needs_rebase => DeliveryPriority::Information,
            _ => DeliveryPriority::NeedsAction,
        };
        let overlaps = if entry.kind == "overlap" {
            overlaps_for(entry, nick)
        } else {
            Vec::new()
        };
        return Some(Delivery {
            sequence: entry.sequence,
            message,
            priority,
            server_identity: None,
            overlaps,
        });
    }
    routed_message(entry, nick, entries, mode)
}

pub(crate) fn routed_message(
    entry: &Entry,
    nick: &str,
    entries: &[Entry],
    mode: crate::MessageMode,
) -> Option<Delivery> {
    if !entry.is_message() {
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
    message_delivery(entry, nick)
}

pub(crate) fn message_delivery(entry: &Entry, nick: &str) -> Option<Delivery> {
    let priority = match (entry.nick.as_str(), entry.intent.as_deref()) {
        ("you", _) | (_, Some("question" | "request")) => DeliveryPriority::NeedsAction,
        _ => DeliveryPriority::Information,
    };
    Some(Delivery {
        sequence: entry.sequence,
        message: message_for(entry, nick)?,
        priority,
        server_identity: None,
        overlaps: Vec::new(),
    })
}

fn overlaps_for(entry: &Entry, nick: &str) -> Vec<Overlap> {
    if let Some(crate::EventPayload::PathClaim { overlaps, .. }) = &entry.payload
        && !overlaps.is_empty()
    {
        return overlaps
            .iter()
            .filter(|overlap| entry.nick == nick || overlap.owner == nick)
            .map(|overlap| Overlap {
                agents: [entry.nick.clone(), overlap.owner.clone()],
                paths: overlap
                    .paths
                    .iter()
                    .map(|path| match overlap.symbols.get(path) {
                        Some(symbols) => format!(
                            "{path}{}",
                            crate::claims::symbol_overlap(&symbols.requester, &symbols.holder)
                        ),
                        None => path.clone(),
                    })
                    .collect(),
            })
            .collect();
    }
    if let Some((pair, paths)) = entry.text.split_once(" on ")
        && let Some((requester, holder)) = pair.split_once(" overlaps ")
    {
        return vec![Overlap {
            agents: [requester.into(), holder.into()],
            paths: if entry.files.is_empty() {
                vec![
                    paths
                        .strip_suffix("; record a share or wait before editing")
                        .unwrap_or(paths)
                        .into(),
                ]
            } else {
                entry.files.clone()
            },
        }];
    }
    Vec::new()
}

pub(crate) fn hold(
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
