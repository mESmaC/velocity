//! The AIRC modal's read model (SPEC.md F14): tab counts, tagged event rows, agent
//! messages, the claims board, both ledgers and the grouped nick list, plus the footer
//! unread badge. Built from the project channel's records and saved thread state; the
//! window only renders it.

use std::collections::{BTreeMap, BTreeSet};

/// One channel record as the modal reads it. `is_event` is decided by the channel
/// (its F14 event tags), so this module never keeps a second copy of the tag list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircRecord {
    sequence: u64,
    time: String,
    kind: String,
    member: String,
    nick: String,
    text: String,
    /// The event log's wording when it differs from the ledger's (a discovery reads
    /// "logged a discovery · …" in Events).
    event_text: Option<String>,
    to: Vec<String>,
    is_event: bool,
    /// A discovery about Velocity itself: its issue title, and the Velocity issue it was
    /// reported on once that is done.
    velocity_issue: Option<(String, Option<u32>)>,
}

impl AircRecord {
    pub fn new(sequence: u64, time: &str, kind: &str, member: &str, nick: &str) -> Self {
        Self {
            sequence,
            time: time.into(),
            kind: kind.into(),
            member: member.into(),
            nick: nick.into(),
            text: String::new(),
            event_text: None,
            to: Vec::new(),
            is_event: false,
            velocity_issue: None,
        }
    }

    pub fn with_text(mut self, text: &str, to: Vec<String>) -> Self {
        self.text = text.into();
        self.to = to;
        self
    }

    pub fn with_event_text(mut self, text: &str) -> Self {
        self.event_text = (text != self.text).then(|| text.into());
        self
    }

    pub fn with_velocity_issue(mut self, title: &str, issue: Option<u32>) -> Self {
        self.velocity_issue = Some((title.into(), issue));
        self
    }

    pub fn as_event(mut self, is_event: bool) -> Self {
        self.is_event = is_event;
        self
    }
}

/// A held claim on the channel's board.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircClaimRecord {
    member: String,
    nick: String,
    task: String,
    files: Vec<String>,
    dependency: Option<(String, String, String)>,
}

impl AircClaimRecord {
    pub fn new(member: &str, nick: &str, task: &str, files: Vec<String>) -> Self {
        Self {
            member: member.into(),
            nick: nick.into(),
            task: task.into(),
            files,
            dependency: None,
        }
    }

    pub fn with_dependency(mut self, holder: &str, nick: &str, file: &str) -> Self {
        self.dependency = Some((holder.into(), nick.into(), file.into()));
        self
    }
}

/// What Velocity's saved state says a channel member is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AircMemberState {
    /// A duty agent (PR duty, scouts): always listed under On duty.
    Duty {
        nick: String,
        detail: String,
        idle: bool,
    },
    /// A goal swarm agent reading AGENTS.md, SPEC.md and the claims board (F16).
    Choosing,
    Working {
        title: String,
    },
    /// `label` is the thread's sidebar meta, e.g. `#12 · in review`.
    Review {
        number: Option<u32>,
        label: String,
    },
    Failed,
    Closed {
        number: Option<u32>,
    },
    Merged {
        number: Option<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircMember {
    member: String,
    state: AircMemberState,
}

impl AircMember {
    pub fn new(member: &str, state: AircMemberState) -> Self {
        Self {
            member: member.into(),
            state,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AircModalInput {
    pub channel: String,
    pub records: Vec<AircRecord>,
    pub claims: Vec<AircClaimRecord>,
    pub members: Vec<AircMember>,
}

impl AircModalInput {
    pub fn new(
        channel: String,
        records: Vec<AircRecord>,
        claims: Vec<AircClaimRecord>,
        members: Vec<AircMember>,
    ) -> Self {
        Self {
            channel,
            records,
            claims,
            members,
        }
    }
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AircTab {
    #[default]
    Events,
    Claims,
    Messages,
    Discoveries,
    PastWork,
}

/// The type tag's colour family (F14: blue, green, amber, red).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AircTone {
    Neutral,
    Claim,
    Info,
    Positive,
    Warning,
    Danger,
    Muted,
}

/// A nick's stable colour: `you`, `velocity`, or one of the reference's ten hues.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NickTone {
    You,
    Velocity,
    Hue(u8),
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AircSpan {
    Text(String),
    Nick {
        nick: String,
        tone: NickTone,
    },
    /// `#n`: opens that PR's thread, or GitHub.
    PullRequest(u32),
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircLineView {
    pub key: String,
    pub time: String,
    pub tag: String,
    pub tone: AircTone,
    /// Empty for lines Velocity posts about nobody in particular.
    pub nick: String,
    pub nick_tone: NickTone,
    pub spans: Vec<AircSpan>,
    pub mention: bool,
    pub from_you: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircClaimFile {
    pub path: String,
    pub overlap: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircClaimRow {
    pub nick: String,
    pub nick_tone: NickTone,
    pub task: String,
    pub state: String,
    pub files: Vec<AircClaimFile>,
    pub dependency: Option<String>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NickDot {
    Active,
    Idle,
    Hold,
    Left,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircNickRow {
    pub message_mode: String,
    pub nick: String,
    pub tone: NickTone,
    pub dot: NickDot,
    pub detail: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircNickGroup {
    pub label: String,
    pub rows: Vec<AircNickRow>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircTabView {
    pub tab: AircTab,
    pub label: String,
    pub count: String,
    pub warn: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AircModalView {
    pub channel: String,
    pub tabs: Vec<AircTabView>,
    pub events: Vec<AircLineView>,
    pub messages: Vec<AircLineView>,
    pub claims_note: String,
    pub claims_warn: bool,
    pub claims: Vec<AircClaimRow>,
    pub discoveries: Vec<AircLineView>,
    pub past_work: Vec<AircLineView>,
    pub nick_groups: Vec<AircNickGroup>,
}

/// The footer badge: lines posted since you last had the modal open, amber when one of
/// them is an overlap or a failure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AircBadge {
    pub unread: usize,
    pub alert: bool,
}

pub fn unread_badge(records: &[AircRecord], seen: u64) -> AircBadge {
    let unread: Vec<_> = records
        .iter()
        .filter(|record| record.sequence > seen && record.nick != "you")
        .filter(|record| record.is_event || record.kind == "message")
        .collect();
    AircBadge {
        unread: unread.len(),
        alert: unread
            .iter()
            .any(|record| record.is_event && matches!(record.kind.as_str(), "overlap" | "failed")),
    }
}

/// The newest record's sequence: what opening the modal marks as read.
pub fn latest_sequence(records: &[AircRecord]) -> u64 {
    records
        .iter()
        .map(|record| record.sequence)
        .max()
        .unwrap_or(0)
}

pub fn modal_view(input: &AircModalInput) -> AircModalView {
    let nicks = known_nicks(input);
    let events: Vec<_> = input
        .records
        .iter()
        .filter(|record| record.is_event)
        .map(|record| event_line(record, &nicks))
        .collect();
    let messages: Vec<_> = input
        .records
        .iter()
        .filter(|record| !record.is_event && record.kind == "message")
        .map(|record| message_line(record, &nicks))
        .collect();
    let ledger = |kind: &str| -> Vec<AircLineView> {
        input
            .records
            .iter()
            .filter(|record| record.kind == kind)
            .map(|record| ledger_line(record, &nicks))
            .collect()
    };
    let (claims, overlap_count) = claim_rows(input);
    AircModalView {
        channel: format!("#{}", input.channel),
        tabs: tabs(
            [events.len(), claims.len(), messages.len()],
            [ledger("discovery").len(), ledger("past_work").len()],
            overlap_count,
        ),
        events,
        messages,
        claims_note: claims_note(overlap_count),
        claims_warn: overlap_count > 0,
        claims,
        discoveries: ledger("discovery"),
        past_work: ledger("past_work"),
        nick_groups: nick_groups(input),
    }
}

fn tabs(logs: [usize; 3], ledgers: [usize; 2], overlaps: usize) -> Vec<AircTabView> {
    let [events, claims, messages] = logs;
    let [discoveries, past_work] = ledgers;
    let tab = |tab, label: &str, count: usize| AircTabView {
        tab,
        label: label.into(),
        count: count.to_string(),
        warn: false,
    };
    let mut claims_tab = tab(AircTab::Claims, "Claims", claims);
    if overlaps > 0 {
        claims_tab.count = format!("{claims} · {overlaps} overlap{}", plural(overlaps));
        claims_tab.warn = true;
    }
    vec![
        tab(AircTab::Events, "Events", events),
        claims_tab,
        tab(AircTab::Messages, "Messages", messages),
        tab(AircTab::Discoveries, "Discoveries", discoveries),
        tab(AircTab::PastWork, "Past work", past_work),
    ]
}

fn claims_note(overlaps: usize) -> String {
    match overlaps {
        0 => "No overlaps. Every active agent has its own files.".into(),
        1 => "1 file is claimed by more than one agent. They record a share or a wait.".into(),
        count => format!(
            "{count} files are claimed by more than one agent. They record a share or a wait."
        ),
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// The reference's stable nick colour: a 31-multiplier hash over the nick's UTF-16
/// leading units, seeded with 7, over ten hues.
pub fn nick_tone(nick: &str) -> NickTone {
    match nick {
        "you" => NickTone::You,
        "velocity" => NickTone::Velocity,
        _ => {
            let hash = nick.chars().fold(7_u32, |hash, character| {
                let mut units = [0_u16; 2];
                let unit = character.encode_utf16(&mut units)[0];
                hash.wrapping_mul(31).wrapping_add(u32::from(unit))
            });
            NickTone::Hue((hash % 10) as u8)
        }
    }
}

/// `hh:mm` from the channel's RFC 3339 UTC time, as the rest of the window shows times.
fn clock(time: &str) -> String {
    time.get(11..16)
        .filter(|clock| clock.as_bytes().get(2) == Some(&b':'))
        .unwrap_or_default()
        .into()
}

fn known_nicks(input: &AircModalInput) -> BTreeSet<String> {
    let mut nicks: BTreeSet<String> = input
        .records
        .iter()
        .filter(|record| matches!(record.kind.as_str(), "join" | "leave"))
        .map(|record| record.nick.clone())
        .collect();
    nicks.extend(input.claims.iter().map(|claim| claim.nick.clone()));
    nicks.extend(
        input
            .members
            .iter()
            .filter_map(|member| match &member.state {
                AircMemberState::Duty { nick, .. } => Some(nick.clone()),
                _ => None,
            }),
    );
    nicks
}

/// Splits a line into text, known nicks and `#n` PR references.
fn spans(text: &str, nicks: &BTreeSet<String>) -> Vec<AircSpan> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut word = String::new();
    let flush_word = |word: &mut String, plain: &mut String, spans: &mut Vec<AircSpan>| {
        if word.is_empty() {
            return;
        }
        let number = plain
            .ends_with('#')
            .then(|| word.parse::<u32>().ok())
            .flatten();
        let span = match number {
            Some(number) => {
                plain.pop();
                Some(AircSpan::PullRequest(number))
            }
            // A branch such as `vel/thread-ipc` names the branch, not the agent.
            None if !plain.ends_with('/') && nicks.contains(word.as_str()) => {
                Some(AircSpan::Nick {
                    nick: word.clone(),
                    tone: nick_tone(word),
                })
            }
            None => None,
        };
        match span {
            Some(span) => {
                if !plain.is_empty() {
                    spans.push(AircSpan::Text(std::mem::take(plain)));
                }
                spans.push(span);
                word.clear();
            }
            None => plain.push_str(&std::mem::take(word)),
        }
    };
    for character in text.chars() {
        if character.is_alphanumeric() || matches!(character, '-' | '_') {
            word.push(character);
        } else {
            flush_word(&mut word, &mut plain, &mut spans);
            plain.push(character);
        }
    }
    flush_word(&mut word, &mut plain, &mut spans);
    if !plain.is_empty() {
        spans.push(AircSpan::Text(plain));
    }
    spans
}

fn line(record: &AircRecord, tag: &str, tone: AircTone, text: &str) -> AircLineView {
    AircLineView {
        key: record.sequence.to_string(),
        time: clock(&record.time),
        tag: tag.into(),
        tone,
        nick: record.nick.clone(),
        nick_tone: nick_tone(&record.nick),
        spans: vec![AircSpan::Text(text.into())],
        mention: false,
        from_you: record.nick == "you",
    }
}

fn event_line(record: &AircRecord, nicks: &BTreeSet<String>) -> AircLineView {
    let mut view = line(record, &record.kind, event_tone(&record.kind), "");
    // Velocity's own lines name both agents in the text; nobody stands in front of them.
    if record.nick == "velocity" {
        view.nick.clear();
    }
    let mut named = nicks.clone();
    named.remove("you");
    view.spans = spans(record.event_text.as_ref().unwrap_or(&record.text), &named);
    view
}

fn message_line(record: &AircRecord, nicks: &BTreeSet<String>) -> AircLineView {
    let mut view = line(record, "", AircTone::Neutral, "");
    // Your `nick:` comments are stored as their body addressed to that nick.
    let text = match record.to.first() {
        Some(recipient) if record.nick == "you" => format!("{recipient}: {}", record.text),
        _ => record.text.clone(),
    };
    view.mention = record.to.iter().any(|recipient| recipient == "you")
        || text.starts_with("you:")
        || text.contains(" you:");
    view.spans = spans(&text, nicks);
    view
}

fn ledger_line(record: &AircRecord, nicks: &BTreeSet<String>) -> AircLineView {
    let mut view = line(record, "", AircTone::Neutral, "");
    view.spans = spans(&record.text, nicks);
    if let Some((title, issue)) = &record.velocity_issue {
        let filed = issue.map_or_else(
            || "filing…".to_owned(),
            |number| format!("{VELOCITY_REPOSITORY}#{number}"),
        );
        view.spans.push(AircSpan::Text(format!(
            " · Velocity issue: {title} · {filed}"
        )));
    }
    view
}

/// Velocity's own repository, where discoveries about Velocity are filed.
const VELOCITY_REPOSITORY: &str = "mESmaC/velocity";

/// F14: blue for PR / issue / task, green for approved / merged / unblocked, amber for
/// overlap / waits / shares / changes / order, red for refused / failed.
pub fn event_tone(tag: &str) -> AircTone {
    match tag {
        "claim" => AircTone::Claim,
        "PR" | "issue" | "task" | "swarm" | "pushed" | "updated" => AircTone::Info,
        "approved" | "merged" | "unblocked" | "rebased" | "proved" => AircTone::Positive,
        "overlap" | "statement" | "waits" | "changes" | "shares" | "order" | "stalled"
        | "at limit" | "reverted" => AircTone::Warning,
        "refused" | "failed" | "stress" | "conflict" | "broken" => AircTone::Danger,
        "left" | "discovery" | "released" => AircTone::Muted,
        _ => AircTone::Neutral,
    }
}

fn claim_state(member: Option<&AircMemberState>) -> (&'static str, Option<u32>) {
    match member {
        Some(AircMemberState::Choosing) => ("choosing", None),
        Some(AircMemberState::Working { .. }) => ("working", None),
        Some(AircMemberState::Review { number, .. }) => ("review", *number),
        Some(AircMemberState::Failed) => ("failed · held", None),
        Some(AircMemberState::Closed { .. }) => ("closed · held", None),
        Some(AircMemberState::Merged { .. }) => ("merged", None),
        Some(AircMemberState::Duty { .. }) | None => ("held", None),
    }
}

/// Claims board rows, and how many files two active (working or PR-open) agents claim.
/// Registration-list claims are shared, so they never count as overlaps (F14).
fn claim_rows(input: &AircModalInput) -> (Vec<AircClaimRow>, usize) {
    let states: BTreeMap<_, _> = input
        .members
        .iter()
        .map(|member| (member.member.as_str(), &member.state))
        .collect();
    let active = |claim: &AircClaimRecord| {
        matches!(
            claim_state(states.get(claim.member.as_str()).copied()).0,
            "working" | "review"
        )
    };
    let mut holders: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for claim in input.claims.iter().filter(|claim| active(claim)) {
        for file in claim
            .files
            .iter()
            .filter(|file| !file.ends_with("::registration"))
        {
            holders.entry(file).or_default().insert(&claim.member);
        }
    }
    let overlapping: BTreeSet<&str> = holders
        .into_iter()
        .filter(|(_, members)| members.len() > 1)
        .map(|(file, _)| file)
        .collect();
    let rows = input
        .claims
        .iter()
        .map(|claim| {
            let (state, number) = claim_state(states.get(claim.member.as_str()).copied());
            AircClaimRow {
                dependency: dependency_label(claim, &states),
                nick: claim.nick.clone(),
                nick_tone: nick_tone(&claim.nick),
                task: claim.task.clone(),
                state: match (state, number) {
                    ("review", Some(number)) => format!("#{number}"),
                    ("review", None) => "PR open".into(),
                    (state, _) => state.into(),
                },
                files: claim
                    .files
                    .iter()
                    .map(|path| AircClaimFile {
                        path: path.clone(),
                        overlap: active(claim) && overlapping.contains(path.as_str()),
                    })
                    .collect(),
            }
        })
        .collect();
    (rows, overlapping.len())
}

fn dependency_label(
    claim: &AircClaimRecord,
    states: &BTreeMap<&str, &AircMemberState>,
) -> Option<String> {
    let (holder, nick, file) = claim.dependency.as_ref()?;
    let (_, number) = claim_state(states.get(holder.as_str()).copied());
    let pull_request = number
        .map(|number| format!(" (#{number})"))
        .unwrap_or_default();
    Some(format!(
        "waits for {nick}{pull_request} to merge · shares {file}"
    ))
}

const GROUPS: [&str; 6] = [
    "On duty",
    "Choosing work",
    "Working",
    "PR open",
    "Holding a claim",
    "Left",
];
/// F14 keeps only the last four agents that left.
const LEFT_SHOWN: usize = 4;

/// Presence in channel order: each member's latest nick and whether it is still joined.
fn presence(records: &[AircRecord]) -> Vec<(String, String, bool)> {
    let mut latest: BTreeMap<&str, (u64, &str, bool)> = BTreeMap::new();
    for record in records
        .iter()
        .filter(|record| matches!(record.kind.as_str(), "join" | "leave"))
    {
        latest.insert(
            &record.member,
            (record.sequence, &record.nick, record.kind == "join"),
        );
    }
    let mut members: Vec<_> = latest
        .into_iter()
        .map(|(member, (sequence, nick, joined))| (sequence, member.into(), nick.into(), joined))
        .collect();
    members.sort_by_key(|(sequence, ..)| *sequence);
    members
        .into_iter()
        .map(|(_, member, nick, joined)| (member, nick, joined))
        .collect()
}

fn nick_groups(input: &AircModalInput) -> Vec<AircNickGroup> {
    let mut groups: [Vec<AircNickRow>; 6] = Default::default();
    let row = |nick: &str, dot, detail: &str| AircNickRow {
        message_mode: input
            .records
            .iter()
            .rev()
            .find(|record| record.nick == nick && record.kind == "message_mode")
            .map(|record| record.text.clone())
            .unwrap_or_else(|| "Open".into()),
        nick: nick.into(),
        tone: nick_tone(nick),
        dot,
        detail: detail.into(),
    };
    for member in &input.members {
        if let AircMemberState::Duty { nick, detail, idle } = &member.state {
            let dot = if *idle {
                NickDot::Idle
            } else {
                NickDot::Active
            };
            groups[0].push(row(nick, dot, detail));
        }
    }
    let states: BTreeMap<_, _> = input
        .members
        .iter()
        .map(|member| (member.member.as_str(), &member.state))
        .collect();
    let claimed: BTreeSet<&str> = input
        .claims
        .iter()
        .map(|claim| claim.member.as_str())
        .collect();
    for (member, nick, joined) in presence(&input.records) {
        let state = states.get(member.as_str()).copied();
        if matches!(state, Some(AircMemberState::Duty { .. })) {
            continue;
        }
        let holds = claimed.contains(member.as_str());
        if let Some((group, entry)) = member_row(state, joined, holds) {
            groups[group].push(row(&nick, entry.0, &entry.1));
        }
    }
    let left = groups[5].len();
    groups[5].drain(..left.saturating_sub(LEFT_SHOWN));
    GROUPS
        .iter()
        .zip(groups)
        .filter(|(_, rows)| !rows.is_empty())
        .map(|(label, rows)| AircNickGroup {
            label: format!("{label} · {}", rows.len()),
            rows,
        })
        .collect()
}

/// Which group a thread member is listed under, with its dot and what it is doing.
fn member_row(
    state: Option<&AircMemberState>,
    joined: bool,
    holds: bool,
) -> Option<(usize, (NickDot, String))> {
    let number = |number: &Option<u32>| {
        number
            .map(|number| format!("#{number} "))
            .unwrap_or_default()
    };
    match state {
        Some(AircMemberState::Choosing) if joined => {
            Some((1, (NickDot::Idle, "reading the claims board".into())))
        }
        Some(AircMemberState::Working { title }) => Some((2, (NickDot::Active, title.clone()))),
        Some(AircMemberState::Review { label, .. }) => Some((3, (NickDot::Active, label.clone()))),
        Some(AircMemberState::Failed) if holds => {
            Some((4, (NickDot::Hold, "failed · claim held".into())))
        }
        Some(AircMemberState::Closed { number: closed }) if holds => {
            Some((4, (NickDot::Hold, format!("{}closed", number(closed)))))
        }
        Some(AircMemberState::Merged { number: merged }) if !joined => {
            Some((5, (NickDot::Left, format!("{}merged", number(merged)))))
        }
        _ if joined => Some((2, (NickDot::Active, String::new()))),
        _ => Some((5, (NickDot::Left, "left".into()))),
    }
}
