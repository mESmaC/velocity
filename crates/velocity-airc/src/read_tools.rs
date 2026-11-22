//! Compact, caller-scoped views of the channel for MCP readers.
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use velocity_core::text_budget::{TOOL_ANSWER_BYTES, json_text_page, preview};

use crate::{
    Channel, Claim, Entry, Error, delivery, mode_for,
    protocol::{ledger, text},
};

const CLAIM_REPLY_BYTES: usize = 1_900;
const CLAIM_PAGE_ENVELOPE_BYTES: usize = 80;
const CLAIM_PAGE_BUDGET: usize = CLAIM_REPLY_BYTES - CLAIM_PAGE_ENVELOPE_BYTES;
const CLAIM_COMPACT_THRESHOLD_BYTES: usize = 1_000;
const CLAIM_MAX_ROW_BYTES: usize = CLAIM_PAGE_BUDGET - 1;
const CLAIM_SUMMARY_NICK_JSON_BYTES: usize = 128;
const CLAIM_SUMMARY_PIECE_JSON_BYTES: usize = 200;
const CLAIM_SUMMARY_CHANGE_JSON_BYTES: usize = 128;
const CLAIM_SUMMARY_TASK_JSON_BYTES: usize = 400;
const MESSAGE_BYTES: usize = 512;
const MESSAGES_PER_READ: usize = 10;
pub(crate) const PAGE_BYTES: usize = TOOL_ANSWER_BYTES - 256;

pub(crate) fn log_fields() -> Value {
    json!({
        "after":{"type":"integer","minimum":0},
        "before":{"type":"integer","minimum":1},
        "limit":{"type":"integer","minimum":1,"default":50},
        "query":{"type":"string"},
        "sequence":{"type":"integer","minimum":1},
        "offset":{"type":"integer","minimum":0}
    })
}

pub(crate) fn feed_fields() -> Value {
    let mut fields = log_fields();
    fields["kind"] = json!({
        "type":"string",
        "description":"Optional entry kind filter (such as merged, rebased, claim, message, checkout), or \"events\" for F14 event-log entries only"
    });
    fields
}

pub(crate) fn log(channel: &Channel, name: &str, arguments: &Value) -> Result<Value, Error> {
    let selected = sequence(arguments, "sequence")?;
    let after = sequence(arguments, "after")?.max(selected.saturating_sub(1));
    let before = sequence(arguments, "before")?;
    let limit = arguments
        .get("limit")
        .map_or(Ok(50), |_| counter(arguments, "limit"))?;
    if limit == 0 {
        return Err(Error::Invalid("limit must be positive".into()));
    }
    let query = arguments
        .get("query")
        .map(|_| text(arguments, "query"))
        .transpose()?
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let mut entries = readable_log_entries(channel, name, arguments, after)?;
    if selected != 0 {
        let entry = entries
            .iter()
            .find(|entry| entry.sequence == selected)
            .ok_or_else(|| Error::Invalid("No readable entry at that sequence".into()))?;
        return log_entry(entry, counter(arguments, "offset")?);
    }
    entries
        .retain(|entry| (before == 0 || entry.sequence < before) && matches_query(entry, &query));
    // Storage and remote routing both preserve sequence order.
    let total = entries.len();
    let mut remaining = PAGE_BYTES;
    let mut rows = Vec::new();
    let mut last = None;
    for entry in entries.iter().rev().take(limit) {
        let mut row = serde_json::to_value(entry)?;
        if row.to_string().len() >= PAGE_BYTES {
            row = json!({"sequence": entry.sequence, "kind": entry.kind,
                "text": preview(&entry.text, 400), "details_omitted": true});
        }
        if !push_row(&mut rows, &mut remaining, row) {
            break;
        }
        last = Some(entry.sequence);
    }
    let more = total - rows.len();
    Ok(json!({"entries": rows, "more": more, "next_before": last.filter(|_| more > 0)}))
}

fn readable_log_entries(
    channel: &Channel,
    name: &str,
    arguments: &Value,
    after: u64,
) -> Result<Vec<Entry>, Error> {
    let kind_filter = arguments
        .get("kind")
        .map(|_| text(arguments, "kind"))
        .transpose()?
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match name {
        "read_feed" if kind_filter == Some("events") => channel.events(after),
        "read_feed" => Ok(channel
            .read(after)?
            .into_iter()
            .filter(|entry| {
                !matches!(
                    entry.kind.as_str(),
                    "delivery_ack" | "message_mode" | "task_list" | "dependency_end"
                ) && kind_filter.is_none_or(|wanted| entry.kind == wanted)
            })
            .collect()),
        "read_events" => channel.events(after),
        _ => channel.ledger(ledger(arguments)?, after),
    }
}

fn matches_query(entry: &Entry, query: &str) -> bool {
    query.is_empty()
        || [&entry.text, &entry.kind, &entry.nick]
            .into_iter()
            .chain(&entry.files)
            .any(|field| field.to_lowercase().contains(query))
        || entry
            .discovery_kind
            .is_some_and(|kind| kind.as_str().contains(query))
        || [
            &entry.velocity_issue,
            &entry.rosen_issue,
            &entry.collapse_issue,
        ]
        .into_iter()
        .flatten()
        .any(|title| title.to_lowercase().contains(query))
        || entry
            .friction
            .as_ref()
            .is_some_and(|friction| friction.tool_or_step.to_lowercase().contains(query))
        || entry.tool_request.as_ref().is_some_and(|request| {
            [
                &request.tool_name,
                &request.would_do,
                &request.would_replace,
                &request.saving_per_use,
            ]
            .into_iter()
            .any(|field| field.to_lowercase().contains(query))
        })
        || entry
            .change_id()
            .is_some_and(|change| change.to_lowercase().contains(query))
        || entry
            .payload
            .as_ref()
            .is_some_and(|payload| payload.matches_query(query))
        || entry.claim.as_ref().is_some_and(|claim| {
            std::iter::once(&claim.task)
                .chain(&claim.files)
                .any(|field| field.to_lowercase().contains(query))
        })
        || entry
            .acknowledged
            .is_some_and(|request| request.to_string() == query)
}

fn log_entry(entry: &Entry, offset: usize) -> Result<Value, Error> {
    let encoded = serde_json::to_string(entry)?;
    if offset == 0 && encoded.len() <= PAGE_BYTES {
        return Ok(json!({"entry": entry}));
    }
    let (data, next_offset) = json_text_page(&encoded, offset, 5_000)
        .ok_or_else(|| Error::Invalid("Use the returned byte offset".into()))?;
    Ok(json!({"sequence": entry.sequence, "entry_json": data, "next_offset": next_offset}))
}

pub(crate) fn push_row(rows: &mut Vec<Value>, remaining: &mut usize, row: Value) -> bool {
    let size = row.to_string().len() + 1;
    if size > *remaining {
        return false;
    }
    *remaining -= size;
    rows.push(row);
    true
}

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
    let rosen = channel.is_rosen_project(member)?;
    let relevant: Vec<_> = claims
        .iter()
        .filter_map(|claim| {
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
                .then_some((claim, own, files))
        })
        .collect();
    if task.is_some()
        && let [(claim, own, files)] = relevant.as_slice()
        && claim_exceeds_page(claim, *own, files, rosen)
    {
        return paged_claim_task(claim, *own, files, offset);
    }
    let rows = relevant
        .into_iter()
        .map(|(claim, own, files)| claim_row(claim, own, &files, rosen));
    Ok(claim_page(counts, rows, offset, filtered))
}

fn physical_path(file: &str) -> &str {
    file.strip_suffix("::registration").unwrap_or(file)
}

fn full_claim_row(claim: &Claim, own: bool, files: &[&String], rosen: bool) -> Value {
    let mut row = json!({
        "nick": claim.nick,
        "task": claim.task,
        "piece": claim.key(),
        "own": own,
    });
    if !rosen || !files.is_empty() {
        row["files"] = json!(files);
    }
    if let Some(change) = &claim.change {
        row["change"] = json!(change);
    }
    row
}

fn compact_claim_row(claim: &Claim, own: bool, files: &[&String]) -> Value {
    // Omit the file list when it pushes the row over budget, keeping the full task
    // text intact while the compact row still fits within one page (#532).
    let mut row = json!({
        "nick": claim.nick,
        "own": own,
        "path_count": files.len(),
        "task": claim.task,
        "piece": claim.key(),
        "details_omitted": true,
    });
    if let Some(change) = &claim.change {
        row["change"] = json!(change);
    }
    row
}

fn summarized_claim_row(claim: &Claim, own: bool, files: &[&String]) -> Value {
    // When even the compact row exceeds a single page, bound every string field in JSON
    // bytes so list pagination always makes progress; callers recover full task text by
    // querying this claim with `task` and paging via `offset` (#677).
    let task_preview = json_preview(&claim.task, CLAIM_SUMMARY_TASK_JSON_BYTES);
    let task_truncated = task_preview != claim.task;
    let mut row = json!({
        "nick": json_preview(&claim.nick, CLAIM_SUMMARY_NICK_JSON_BYTES),
        "own": own,
        "path_count": files.len(),
        "piece": json_preview(claim.key(), CLAIM_SUMMARY_PIECE_JSON_BYTES),
        "task": task_preview,
        "details_omitted": true,
    });
    if task_truncated {
        row["task_truncated"] = json!(true);
    }
    if let Some(change) = &claim.change {
        row["change"] = json!(json_preview(change, CLAIM_SUMMARY_CHANGE_JSON_BYTES));
    }
    row
}

fn claim_exceeds_page(claim: &Claim, own: bool, files: &[&String], rosen: bool) -> bool {
    let full_len = full_claim_row(claim, own, files, rosen).to_string().len();
    if full_len <= CLAIM_COMPACT_THRESHOLD_BYTES
        || (files.is_empty() && full_len <= CLAIM_MAX_ROW_BYTES)
    {
        return false;
    }
    compact_claim_row(claim, own, files).to_string().len() > CLAIM_MAX_ROW_BYTES
}

fn claim_row(claim: &Claim, own: bool, files: &[&String], rosen: bool) -> Value {
    let row = full_claim_row(claim, own, files, rosen);
    let row_len = row.to_string().len();
    if row_len <= CLAIM_COMPACT_THRESHOLD_BYTES
        || (files.is_empty() && row_len <= CLAIM_MAX_ROW_BYTES)
    {
        return row;
    }
    let compact = compact_claim_row(claim, own, files);
    if compact.to_string().len() <= CLAIM_MAX_ROW_BYTES {
        return compact;
    }
    summarized_claim_row(claim, own, files)
}

fn paged_claim_task(
    claim: &Claim,
    own: bool,
    files: &[&String],
    offset: usize,
) -> Result<Value, Error> {
    let mut row = json!({
        "nick": json_preview(&claim.nick, CLAIM_SUMMARY_NICK_JSON_BYTES),
        "own": own,
        "path_count": files.len(),
        "piece": json_preview(claim.key(), CLAIM_SUMMARY_PIECE_JSON_BYTES),
        "task": "",
        "task_offset": offset,
        "details_omitted": true,
    });
    if let Some(change) = &claim.change {
        row["change"] = json!(json_preview(change, CLAIM_SUMMARY_CHANGE_JSON_BYTES));
    }
    let next_placeholder = claim.task.len().max(9_999);
    let overhead = json!({"claims": [row], "next_offset": next_placeholder})
        .to_string()
        .len();
    let task_budget = CLAIM_REPLY_BYTES.saturating_sub(overhead);
    let (task_chunk, next_offset) = json_text_page(&claim.task, offset, task_budget)
        .ok_or_else(|| Error::Invalid("Use the returned byte offset".into()))?;
    row["task"] = json!(task_chunk);
    Ok(json!({"claims": [row], "next_offset": next_offset}))
}

fn json_encoded_len(text: &str) -> usize {
    text.chars()
        .map(|character| match character {
            '\\' | '"' | '\n' | '\r' | '\t' => 2,
            '\0'..='\u{001f}' => 6,
            _ => character.len_utf8(),
        })
        .sum()
}

fn json_preview(text: &str, max_json_bytes: usize) -> String {
    if json_encoded_len(text) <= max_json_bytes {
        return text.to_owned();
    }
    let marker = "…";
    let budget = max_json_bytes.saturating_sub(marker.len());
    let prefix = json_text_page(text, 0, budget)
        .map(|(chunk, _)| chunk)
        .unwrap_or_default();
    format!("{prefix}{marker}")
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
    let mut remaining = CLAIM_REPLY_BYTES - CLAIM_PAGE_ENVELOPE_BYTES;
    let count_length = counts.len();
    for (nick, count) in counts.into_iter().skip(offset) {
        let entry_nick = if json!([nick, count]).to_string().len() > CLAIM_PAGE_BUDGET {
            json_preview(nick, CLAIM_SUMMARY_NICK_JSON_BYTES)
        } else {
            nick.to_owned()
        };
        // The array spelling is just longer than the corresponding map entry.
        let size = json!([&entry_nick, count]).to_string().len();
        if size > remaining {
            return json!({"counts": page_counts, "claims": page_claims, "next_offset": index});
        }
        remaining -= size;
        page_counts.insert(entry_nick, count);
        index += 1;
    }
    let mut next = None;
    for claim in claims.skip(offset.saturating_sub(count_length)) {
        let size = claim.to_string().len() + 1;
        if size > remaining {
            if page_counts.is_empty() && page_claims.is_empty() {
                let fallback = json!({"details_omitted": true, "task_truncated": true});
                remaining = remaining.saturating_sub(fallback.to_string().len() + 1);
                page_claims.push(fallback);
                index += 1;
                continue;
            }
            next = Some(index);
            break;
        }
        remaining -= size;
        page_claims.push(claim);
        index += 1;
    }
    if filtered {
        json!({"claims": page_claims, "next_offset": next})
    } else {
        json!({"counts": page_counts, "claims": page_claims, "next_offset": next})
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
        let unread = readable_messages(channel, nick, &history, after.max(native_after))?
            .into_iter()
            .filter(|entry| {
                !acknowledged.contains(&entry.sequence) && !self.seen.contains(&entry.sequence)
            })
            .take(MESSAGES_PER_READ + 1)
            .collect::<Vec<_>>();
        let mut pages = Vec::new();
        let mut remaining = PAGE_BYTES;
        for entry in unread.iter().take(MESSAGES_PER_READ) {
            if !push_row(&mut pages, &mut remaining, message_page(entry, 0)?) {
                break;
            }
            self.seen.insert(entry.sequence);
        }
        Ok(json!({"messages": pages, "more_unread": pages.len() < unread.len()}))
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
    Ok(json!({"sequence": entry.sequence, "nick": entry.nick,
        "intent": entry.intent.as_deref().map(|intent| preview(intent, 200)),
        "text": text, "next_offset": next_offset}))
}

fn sequence(arguments: &Value, field: &str) -> Result<u64, Error> {
    arguments.get(field).map_or(Ok(0), |value| {
        value
            .as_u64()
            .ok_or_else(|| Error::Invalid(format!("{field} must be an unsigned integer")))
    })
}

pub(crate) fn counter(arguments: &Value, field: &str) -> Result<usize, Error> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waits::Checkout;

    fn temp_channel(
        label: &str,
    ) -> Result<(std::path::PathBuf, Channel), Box<dyn std::error::Error>> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "velocity-airc-read-{label}-{}-{stamp}",
            std::process::id()
        ));
        let channel = Channel::open(&directory, "project")?;
        Ok((directory, channel))
    }

    fn collect_task_pages(
        channel: &Channel,
        member: &str,
        task_filter: &str,
    ) -> Result<(String, usize), Box<dyn std::error::Error>> {
        let mut recovered = String::new();
        let mut offset = 0usize;
        let mut pages = 0usize;
        loop {
            let page = claims(
                channel,
                member,
                &json!({"task": task_filter, "offset": offset}),
            )?;
            assert!(
                page.to_string().len() <= CLAIM_REPLY_BYTES,
                "page {pages} exceeded budget: {} bytes",
                page.to_string().len()
            );
            let rows = page["claims"].as_array().ok_or("missing claims array")?;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["task_offset"].as_u64(), Some(offset as u64));
            let chunk = rows[0]["task"].as_str().ok_or("missing task chunk")?;
            recovered.push_str(chunk);
            pages += 1;
            match page["next_offset"].as_u64() {
                Some(next) => {
                    let next = usize::try_from(next)?;
                    assert!(next > offset, "pagination stalled at offset {offset}");
                    offset = next;
                }
                None => break,
            }
        }
        Ok((recovered, pages))
    }

    // Risk (#677): an oversized claim description whose compact row alone exceeds the
    // 1,820-byte page capacity must never trap `airc_claims` pagination at the same
    // offset; exact-piece lookups must page the full UTF-8 task text (including Unicode
    // and JSON-escaped characters) within F14's 2 kB page budget, and unfiltered queries
    // must summarize the oversized row so subsequent claims remain reachable.
    #[test]
    fn oversized_claim_pages_full_text_and_unfiltered_list_advances()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, channel) = temp_channel("oversized-677")?;
        let change = "zrqrpzzruukpynwnvyporzrwpxplrvqq";
        channel.register_checkout(
            "claim-text-50",
            Checkout::rosen(
                "repo".into(),
                "ws/50".into(),
                Some(change.into()),
                "master".into(),
            ),
        )?;
        channel.join("claim-text-50", "claim-text-50")?;
        let ascii_task = "Keep this complete claim description readable in AIRC. ".repeat(45);
        channel.claim_piece(
            "claim-text-50",
            "issue:532",
            &ascii_task,
            "vel/532",
            Vec::new(),
        )?;
        channel.claim_piece(
            "claim-text-50",
            "issue:678",
            "Follow-up claim after oversized claim",
            "vel/678",
            Vec::new(),
        )?;

        let (recovered, pages) = collect_task_pages(&channel, "claim-text-50", "issue:532")?;
        assert!(pages >= 2);
        assert_eq!(recovered, ascii_task);

        let mut seen_pieces = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = claims(&channel, "claim-text-50", &json!({"offset": offset}))?;
            assert!(page.to_string().len() <= CLAIM_REPLY_BYTES);
            for row in page["claims"].as_array().ok_or("missing claims array")? {
                if let Some(piece) = row["piece"].as_str() {
                    seen_pieces.push(piece.to_owned());
                    if piece == "issue:532" {
                        assert_eq!(row["details_omitted"].as_bool(), Some(true));
                        assert_eq!(row["task_truncated"].as_bool(), Some(true));
                        assert_eq!(row["change"].as_str(), Some(change));
                    }
                }
            }
            match page["next_offset"].as_u64() {
                Some(next) => {
                    let next = usize::try_from(next)?;
                    assert!(next > offset, "unfiltered pagination stalled at {offset}");
                    offset = next;
                }
                None => break,
            }
        }
        assert!(seen_pieces.contains(&"issue:532".to_owned()));
        assert!(seen_pieces.contains(&"issue:678".to_owned()));

        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#677): multi-byte Unicode, JSON-escaped control characters, and legacy claims
    // keyed on their oversized task text must still fit strictly within the JSON reply
    // budget, recover verbatim across pages, and reject mid-character byte offsets.
    #[test]
    fn oversized_unicode_and_legacy_claims_respect_json_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, channel) = temp_channel("unicode-677")?;
        channel.join("unicode-agent", "unicode-agent")?;
        let unicode_task = "🚀 日本語 — café \"quoted\" \\ \u{0001} ".repeat(110);
        channel.claim_piece(
            "unicode-agent",
            "issue:677",
            &unicode_task,
            "vel/677",
            Vec::new(),
        )?;
        let legacy_task = "Legacy task without separate piece key 🦀 ".repeat(60);
        channel.claim("unicode-agent", &legacy_task, "vel/legacy", Vec::new())?;

        let (recovered_unicode, unicode_pages) =
            collect_task_pages(&channel, "unicode-agent", "issue:677")?;
        assert!(unicode_pages >= 2);
        assert_eq!(recovered_unicode, unicode_task);

        let invalid = claims(
            &channel,
            "unicode-agent",
            &json!({"task": "issue:677", "offset": 1}),
        );
        assert!(matches!(invalid, Err(Error::Invalid(_))));

        let (recovered_legacy, legacy_pages) =
            collect_task_pages(&channel, "unicode-agent", &legacy_task)?;
        assert!(legacy_pages >= 2);
        assert_eq!(recovered_legacy, legacy_task);

        let list_page = claims(&channel, "unicode-agent", &json!({}))?;
        assert!(list_page.to_string().len() <= CLAIM_REPLY_BYTES);
        assert_eq!(
            list_page["claims"]
                .as_array()
                .ok_or("missing claims array")?
                .len(),
            2
        );

        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
