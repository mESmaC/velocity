//! Compact, caller-scoped views of the channel for MCP readers.
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::{Channel, Claim, Entry, Error, delivery, mode_for, protocol::text};

const CLAIM_REPLY_BYTES: usize = 1_900;
const MESSAGE_BYTES: usize = 512;
const MESSAGES_PER_READ: usize = 10;

pub(crate) fn claims(channel: &Channel, member: &str, arguments: &Value) -> Result<Value, Error> {
    let path = arguments
        .get("path")
        .map(|_| text(arguments, "path").map(crate::claims::normalize_file))
        .transpose()?;
    let task = arguments
        .get("task")
        .map(|_| text(arguments, "task").map(crate::piece_key))
        .transpose()?;
    let claims = channel.claims()?;
    let offset = counter(arguments, "offset")?;
    let mut counts = BTreeMap::new();
    let mut held_paths = BTreeSet::new();
    let filtered = path.is_some() || task.is_some();
    if !filtered {
        for claim in &claims {
            *counts.entry(claim.nick.as_str()).or_insert(0usize) += 1;
            if claim.member == member {
                held_paths.extend(claim.files.iter().map(|file| physical_path(file)));
            }
        }
    }
    let relevant = claims.iter().filter_map(|claim| {
        if task
            .as_ref()
            .is_some_and(|task| claim.key() != task && crate::piece_key(&claim.task) != *task)
        {
            return None;
        }
        let own = claim.member == member;
        let files: Vec<_> = claim
            .files
            .iter()
            .filter(|file| {
                path.as_ref().map_or_else(
                    || filtered || own || held_paths.contains(physical_path(file)),
                    |path| physical_path(file) == physical_path(path),
                )
            })
            .collect();
        ((!files.is_empty()) || (path.is_none() && (own || filtered)))
            .then(|| claim_row(claim, own, &files))
    });
    Ok(claim_page(counts, relevant, offset, filtered))
}

fn physical_path(file: &str) -> &str {
    file.strip_suffix("::registration").unwrap_or(file)
}

fn claim_row(claim: &Claim, own: bool, files: &[&String]) -> Value {
    let mut row = json!({"nick": claim.nick, "task": claim.task, "piece": claim.key(),
        "own": own, "files": files});
    if row.to_string().len() > 1_000 {
        let preview = |text: &str, limit| text[..text.floor_char_boundary(limit)].to_owned();
        row = json!({"nick": claim.nick, "own": own, "path_count": files.len(),
            "task": preview(&claim.task, 48), "piece": preview(claim.key(), 80),
            "details_omitted": true});
    }
    row
}

fn claim_page(
    counts: BTreeMap<&str, usize>,
    claims: impl Iterator<Item = Value>,
    offset: usize,
    filtered: bool,
) -> Value {
    let mut page_counts = BTreeMap::new();
    let mut page_claims = Vec::new();
    let mut index = offset;
    let mut remaining = CLAIM_REPLY_BYTES - 80;
    let count_length = counts.len();
    for (nick, count) in counts.into_iter().skip(offset) {
        // The array spelling is just longer than the corresponding map entry.
        let size = json!([nick, count]).to_string().len();
        if size > remaining {
            return json!({"counts":page_counts, "claims":page_claims, "next_offset":index});
        }
        remaining -= size;
        page_counts.insert(nick, count);
        index += 1;
    }
    let mut next = None;
    for claim in claims.skip(offset.saturating_sub(count_length)) {
        let size = claim.to_string().len() + 1;
        if size > remaining {
            next = Some(index);
            break;
        }
        remaining -= size;
        page_claims.push(claim);
        index += 1;
    }
    if filtered {
        json!({"claims":page_claims, "next_offset":next})
    } else {
        json!({"counts":page_counts, "claims":page_claims, "next_offset":next})
    }
}

#[derive(Default)]
pub(crate) struct MessageReader {
    seen: BTreeSet<u64>,
}

impl MessageReader {
    pub(crate) fn read(
        &mut self,
        channel: &Channel,
        member: &str,
        nick: &str,
        arguments: &Value,
    ) -> Result<Value, Error> {
        let history = channel.snapshot()?;
        let joined = channel.delivery_cursor(member, 0)?;
        if arguments.get("sequence").is_some() {
            let sequence = sequence(arguments, "sequence")?;
            let entry = readable_messages(
                channel,
                nick,
                &history,
                joined.max(sequence.saturating_sub(1)),
            )?
            .into_iter()
            .find(|entry| entry.sequence == sequence)
            .ok_or_else(|| Error::Invalid("No readable message at that sequence".into()))?;
            return message_page(&entry, counter(arguments, "offset")?);
        }
        let after = joined.max(sequence(arguments, "after")?);
        let acknowledged: BTreeSet<_> = history
            .iter()
            .filter(|entry| entry.nick == nick)
            .filter_map(|entry| entry.acknowledged)
            .collect();
        let native_after = channel
            .remote
            .as_ref()
            .map(|remote| remote.acknowledged_cursor(nick))
            .transpose()?
            .unwrap_or(0);
        let mut unread = readable_messages(channel, nick, &history, after.max(native_after))?
            .into_iter()
            .filter(|entry| {
                !acknowledged.contains(&entry.sequence) && !self.seen.contains(&entry.sequence)
            })
            .take(MESSAGES_PER_READ + 1)
            .collect::<Vec<_>>();
        let more = unread.len() > MESSAGES_PER_READ;
        unread.truncate(MESSAGES_PER_READ);
        let pages = unread
            .iter()
            .map(|entry| message_page(entry, 0))
            .collect::<Result<Vec<_>, _>>()?;
        self.seen.extend(unread.iter().map(|entry| entry.sequence));
        Ok(json!({"messages": pages, "more_unread": more}))
    }
}

fn readable_messages(
    channel: &Channel,
    nick: &str,
    history: &[Entry],
    after: u64,
) -> Result<Vec<Entry>, Error> {
    if let Some(remote) = &channel.remote {
        return Ok(remote
            .targeted(nick, after, false)?
            .into_iter()
            .filter(|entry| entry.is_message() && entry.nick != nick)
            .collect());
    }
    let mode = mode_for(history, nick);
    Ok(history
        .iter()
        .filter(|entry| {
            entry.sequence > after
                && entry.is_message()
                && delivery::routed_message(entry, nick, history, mode).is_some()
                && !delivery::hold(entry, nick, history, mode, false)
        })
        .cloned()
        .collect())
}

fn message_page(entry: &Entry, offset: usize) -> Result<Value, Error> {
    let (text, next_offset) = chunk(&entry.text, offset, MESSAGE_BYTES)?;
    Ok(
        json!({"sequence": entry.sequence, "nick": entry.nick, "intent": entry.intent,
        "text": text, "next_offset": next_offset}),
    )
}

fn sequence(arguments: &Value, field: &str) -> Result<u64, Error> {
    arguments.get(field).map_or(Ok(0), |value| {
        value
            .as_u64()
            .ok_or_else(|| Error::Invalid(format!("{field} must be an unsigned integer")))
    })
}

fn counter(arguments: &Value, field: &str) -> Result<usize, Error> {
    usize::try_from(sequence(arguments, field)?)
        .map_err(|_| Error::Invalid(format!("{field} is too large")))
}

fn chunk(text: &str, offset: usize, limit: usize) -> Result<(&str, Option<usize>), Error> {
    if !text.is_char_boundary(offset) {
        return Err(Error::Invalid(
            "Use the byte offset returned by the previous page".into(),
        ));
    }
    let end = text.floor_char_boundary(offset.saturating_add(limit));
    Ok((&text[offset..end], (end < text.len()).then_some(end)))
}
