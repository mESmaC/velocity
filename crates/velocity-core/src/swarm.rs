//! Swarm read models and pure policies (SPEC.md F10): one agent per open item, all started
//! at once. Members are ordinary threads; the swarm is a view over them.

use crate::layout_view::ProviderColor;
use crate::state::{ReviewStage, ThreadState};

/// Where a swarm takes its items from (F10 Source).
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmSource {
    #[default]
    Tasks,
    Issues,
    Both,
}

impl SwarmSource {
    /// The swarm's title suffix, as the reference names a new swarm.
    pub fn label(self) -> &'static str {
        match self {
            Self::Tasks => "Task List",
            Self::Issues => "open issues",
            Self::Both => "Task List + issues",
        }
    }
}

/// The open Swarm dialog: which source it starts on, and the swarm + Items adds to.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwarmDialogRequest {
    pub source: SwarmSource,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number::optional"))]
    #[cfg_attr(feature = "bindings", specta(type = Option<String>))]
    pub into: Option<usize>,
}

impl SwarmDialogRequest {
    pub fn new(source: SwarmSource, into: Option<usize>) -> Self {
        Self { source, into }
    }
}

/// Where one member is in its lifecycle: the segment colours of the progress bars.
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmStage {
    /// A goal swarm agent reading the claims board before it claims a piece (F16).
    Choosing,
    Working,
    PullRequest,
    Fixing,
    Merged,
    Failed,
}

impl SwarmStage {
    const ORDER: [Self; 5] = [
        Self::Working,
        Self::PullRequest,
        Self::Fixing,
        Self::Merged,
        Self::Failed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Choosing => "choosing work",
            Self::Working => "working",
            Self::PullRequest => "PR open",
            Self::Fixing => "addressing review",
            Self::Merged => "merged",
            Self::Failed => "failed",
        }
    }
}

/// One dot of a card's work · PR · CI · review · merged track.
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackStep {
    Todo,
    Running,
    Done,
    Failed,
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmEventKind {
    You,
    Opened,
    Changes,
    Merged,
    Rebase,
    Failed,
    Finished,
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwarmFeedEvent {
    pub timestamp: String,
    pub actor: String,
    pub kind: SwarmEventKind,
    pub message: String,
}

impl SwarmFeedEvent {
    pub fn new(timestamp: &str, actor: &str, kind: SwarmEventKind, message: String) -> Self {
        Self {
            timestamp: timestamp.to_owned(),
            actor: actor.to_owned(),
            kind,
            message,
        }
    }
}

/// What a member's card is built from, read from its saved thread.
#[derive(Clone, Debug)]
pub struct MemberProgress {
    pub identifier: usize,
    pub title: String,
    /// `#n` for an issue, `task` for a Task List item.
    pub reference: String,
    pub issue: bool,
    pub agent_number: u32,
    pub provider: ProviderColor,
    pub model_name: String,
    pub state: ThreadState,
    pub review_stage: Option<ReviewStage>,
    pub pull_request_number: Option<u32>,
    pub activity_label: String,
    pub added: u32,
    pub deleted: u32,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct SwarmCard {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub title: String,
    pub reference: String,
    pub issue: bool,
    pub stage: SwarmStage,
    pub track: Vec<TrackStep>,
    pub doing: String,
    pub provider: ProviderColor,
    pub agent_label: String,
    pub added: u32,
    pub deleted: u32,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct SwarmLegendItem {
    pub stage: SwarmStage,
    pub count: u32,
    pub label: String,
}

/// The swarm's sidebar row and view, as the store reads it from `state.json`.
#[derive(Clone, Debug)]
pub struct SwarmEntry {
    pub identifier: usize,
    pub source: SwarmSource,
    pub title: String,
    pub started_label: String,
    pub members: Vec<MemberProgress>,
    pub feed: Vec<SwarmFeedEvent>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct SwarmViewSnapshot {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub title: String,
    pub agents_label: String,
    pub started_label: String,
    pub merged_label: Option<String>,
    pub segments: Vec<SwarmStage>,
    pub legend: Vec<SwarmLegendItem>,
    pub cards: Vec<SwarmCard>,
    pub feed: Vec<SwarmFeedEvent>,
    pub composer_placeholder: String,
}

pub fn member_stage(state: ThreadState, review_stage: Option<ReviewStage>) -> SwarmStage {
    match state {
        ThreadState::Merged | ThreadState::Done => SwarmStage::Merged,
        ThreadState::Error | ThreadState::Closed => SwarmStage::Failed,
        ThreadState::Review if review_stage == Some(ReviewStage::Fixing) => SwarmStage::Fixing,
        ThreadState::Review => SwarmStage::PullRequest,
        ThreadState::Draft | ThreadState::Spec | ThreadState::Working => SwarmStage::Working,
    }
}

/// work · PR · CI · review · merged, as the reference's `swCard` lights them.
pub fn member_track(member: &MemberProgress) -> Vec<TrackStep> {
    use TrackStep::{Done, Failed, Running, Todo};
    let merged = matches!(member.state, ThreadState::Merged | ThreadState::Done);
    let stage = member.review_stage;
    let has_pull_request = member.pull_request_number.is_some() || merged;
    let work = match member.state {
        ThreadState::Working | ThreadState::Draft | ThreadState::Spec => Running,
        ThreadState::Error | ThreadState::Closed if !has_pull_request => Failed,
        _ => Done,
    };
    let opened = if has_pull_request { Done } else { Todo };
    let checks = match stage {
        _ if !has_pull_request => Todo,
        Some(ReviewStage::ContinuousIntegration) if !merged => Running,
        _ => Done,
    };
    let review = match stage {
        _ if merged => Done,
        Some(ReviewStage::Approved) => Done,
        Some(ReviewStage::Fixing) => Failed,
        Some(ReviewStage::Review) => Running,
        _ => Todo,
    };
    let landed = match stage {
        _ if merged => Done,
        Some(ReviewStage::Approved) => Running,
        _ => Todo,
    };
    vec![work, opened, checks, review, landed]
}

/// The card's "what it is doing now" line.
pub fn member_doing(member: &MemberProgress) -> String {
    if member.activity_label == "stalled" || member.activity_label.starts_with("at limit") {
        return member.activity_label.clone();
    }
    let number = member.pull_request_number.unwrap_or_default();
    match member_stage(member.state, member.review_stage) {
        SwarmStage::Choosing | SwarmStage::Working => member.activity_label.clone(),
        SwarmStage::Merged => format!("merged · PR #{number}"),
        SwarmStage::Failed if member.state == ThreadState::Closed => {
            format!("PR #{number} closed · worktree kept")
        }
        SwarmStage::Failed => "failed · worktree kept".into(),
        SwarmStage::Fixing => format!("PR #{number} · addressing review"),
        SwarmStage::PullRequest => match member.review_stage {
            Some(ReviewStage::ContinuousIntegration) => format!("PR #{number} · waiting on CI"),
            Some(ReviewStage::Approved) => format!("PR #{number} · approved · merging"),
            _ => format!("PR #{number} · in review"),
        },
    }
}

pub fn member_card(member: &MemberProgress) -> SwarmCard {
    SwarmCard {
        identifier: member.identifier,
        title: member.title.clone(),
        reference: member.reference.clone(),
        issue: member.issue,
        stage: member_stage(member.state, member.review_stage),
        track: member_track(member),
        doing: member_doing(member),
        provider: member.provider,
        agent_label: format!("a{} · {}", member.agent_number, member.model_name),
        added: member.added,
        deleted: member.deleted,
    }
}

pub fn stage_counts(members: &[MemberProgress]) -> Vec<SwarmLegendItem> {
    SwarmStage::ORDER
        .into_iter()
        .filter_map(|stage| {
            let count = members
                .iter()
                .filter(|member| member_stage(member.state, member.review_stage) == stage)
                .count();
            (count > 0).then(|| SwarmLegendItem {
                stage,
                count: u32::try_from(count).unwrap_or(u32::MAX),
                label: format!("{count} {}", stage.label()),
            })
        })
        .collect()
}

pub fn segments(members: &[MemberProgress]) -> Vec<SwarmStage> {
    members
        .iter()
        .map(|member| member_stage(member.state, member.review_stage))
        .collect()
}

fn merged_count(members: &[MemberProgress]) -> usize {
    segments(members)
        .into_iter()
        .filter(|stage| *stage == SwarmStage::Merged)
        .count()
}

/// The sidebar row's meta line: counts by stage, ending with merged/total.
pub fn sidebar_summary(members: &[MemberProgress]) -> String {
    if members.is_empty() {
        return "no agents".into();
    }
    let merged = merged_count(members);
    let mut parts: Vec<String> = stage_counts(members)
        .into_iter()
        .filter(|item| item.stage != SwarmStage::Merged)
        .map(|item| item.label)
        .collect();
    parts.push(format!("{merged}/{} merged", members.len()));
    parts.join(" · ")
}

pub fn project_swarm_view(entry: &SwarmEntry) -> SwarmViewSnapshot {
    let total = entry.members.len();
    let merged = merged_count(&entry.members);
    SwarmViewSnapshot {
        identifier: entry.identifier,
        title: entry.title.clone(),
        agents_label: format!(
            "{total} agent{} · {} worktree{} live",
            if total == 1 { "" } else { "s" },
            total - merged,
            if total - merged == 1 { "" } else { "s" }
        ),
        started_label: format!("started {}", entry.started_label),
        merged_label: (merged > 0).then(|| format!("{merged}/{total} merged")),
        segments: segments(&entry.members),
        legend: stage_counts(&entry.members),
        cards: entry.members.iter().map(member_card).collect(),
        feed: entry.feed.iter().rev().take(40).rev().cloned().collect(),
        composer_placeholder:
            "Message every agent in the swarm, e.g. \u{201c}use the error type from #41\u{201d}"
                .into(),
    }
}

/// The model for each item, in item order: several models are assigned round-robin (F10).
pub fn round_robin(item_count: usize, model_count: usize) -> Vec<usize> {
    if model_count == 0 {
        return Vec::new();
    }
    (0..item_count).map(|index| index % model_count).collect()
}

/// "3 on Opus · 3 on Sonnet · 3 on Codex", or `None` with fewer than two models.
pub fn split_label(item_count: usize, model_names: &[String]) -> Option<String> {
    if model_names.len() < 2 {
        return None;
    }
    let assigned = round_robin(item_count, model_names.len());
    Some(
        model_names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let count = assigned.iter().filter(|model| **model == index).count();
                format!("{count} on {name}")
            })
            .collect::<Vec<_>>()
            .join(" · "),
    )
}

/// Reviewers PR duty is short of, aiming at one per three open PRs (F10 PR duty capacity).
pub fn pr_duty_shortfall(
    new_pull_requests: usize,
    open_pull_requests: usize,
    reviewers: usize,
) -> usize {
    (new_pull_requests + open_pull_requests)
        .div_ceil(3)
        .saturating_sub(reviewers)
}

/// The dialog's PR duty line, or `None` while nothing is selected.
pub fn pr_duty_capacity_label(
    new_pull_requests: usize,
    open_pull_requests: usize,
    reviewers: usize,
) -> Option<String> {
    if new_pull_requests == 0 {
        return None;
    }
    let pull_requests = new_pull_requests + open_pull_requests;
    let shortfall = pr_duty_shortfall(new_pull_requests, open_pull_requests, reviewers);
    let agents = |count: usize| format!("{count} agent{}", if count == 1 { "" } else { "s" });
    Some(match (shortfall, reviewers) {
        (0, _) => format!(
            "PR duty has {} · enough for {pull_requests} open PRs.",
            agents(reviewers)
        ),
        (_, 0) => format!(
            "Start PR duty with {} · without it the PRs wait as \u{201c}needs a reviewer\u{201d}",
            agents(shortfall)
        ),
        _ => format!(
            "Add {} to PR duty · {reviewers} reviewer{} for {pull_requests} open PRs is the bottleneck",
            agents(shortfall),
            if reviewers == 1 { "" } else { "s" }
        ),
    })
}

/// A member PR's lifecycle as the F6 watcher records it: number, stage, worktree deleted.
#[derive(Clone, Copy, Debug)]
pub struct PullRequestStep<'a> {
    pub number: u32,
    pub stage: &'a str,
    pub worktree_deleted: bool,
}

impl<'a> PullRequestStep<'a> {
    pub fn new(number: u32, stage: &'a str, worktree_deleted: bool) -> Self {
        Self {
            number,
            stage,
            worktree_deleted,
        }
    }
}

/// The swarm feed line for a member PR moving from `before` to `after` (F10 Activity):
/// PR opened, changes requested, comments addressed, merged (with what merging finished),
/// and closed without merging.
pub fn pull_request_event(
    before: Option<PullRequestStep<'_>>,
    after: PullRequestStep<'_>,
    finished_item: &str,
) -> Option<(SwarmEventKind, String)> {
    let number = after.number;
    if number == 0 {
        return None;
    }
    let previous = before.filter(|step| step.number != 0);
    let was = |stage: &str| previous.is_some_and(|step| step.stage == stage);
    let merged = |step: PullRequestStep<'_>| step.stage == "merged" && step.worktree_deleted;
    if merged(after) && !previous.is_some_and(merged) {
        return Some((
            SwarmEventKind::Merged,
            format!("merged #{number} · {finished_item}"),
        ));
    }
    if previous.is_none() {
        return Some((SwarmEventKind::Opened, format!("opened PR #{number}")));
    }
    match after.stage {
        "fixing" if !was("fixing") => Some((
            SwarmEventKind::Changes,
            format!("changes requested on #{number}"),
        )),
        "ci" | "review" | "approved" if was("fixing") => Some((
            SwarmEventKind::Changes,
            format!("review comments resolved on #{number}"),
        )),
        "closed" if !was("closed") => Some((
            SwarmEventKind::Failed,
            format!("PR #{number} closed without merging · worktree kept"),
        )),
        _ => None,
    }
}
