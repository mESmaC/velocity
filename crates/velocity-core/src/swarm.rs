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
    /// Claimed and editing in its workspace.
    Working,
    /// The agent named its change; Rosen lands it once it combines cleanly.
    Named,
    /// A refused land or a post-land revert reopened the change for its owner.
    Fixing,
    Landed,
    Failed,
}

impl SwarmStage {
    const ORDER: [Self; 5] = [
        Self::Working,
        Self::Named,
        Self::Fixing,
        Self::Landed,
        Self::Failed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Choosing => "choosing work",
            Self::Working => "editing",
            Self::Named => "named",
            Self::Fixing => "reopened",
            Self::Landed => "landed",
            Self::Failed => "failed",
        }
    }
}

/// One dot of a card's editing · named · landed track.
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackStep {
    Todo,
    Running,
    Done,
    Failed,
}

#[cfg(feature = "bindings")]
#[derive(serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename = "SwarmEventKind", rename_all = "snake_case")]
enum SwarmEventKindBinding {
    You,
    /// A member named its change.
    Named,
    /// A change was refused, reverted or reopened.
    Changes,
    Landed,
    Failed,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmEventKind {
    You,
    /// A member named its change. Feeds saved before Rosen call it `opened`.
    #[serde(alias = "opened")]
    Named,
    /// A change was refused, reverted or reopened. Saved `rebase` lines from earlier
    /// feeds read as this kind, so old state still loads.
    #[serde(alias = "rebase")]
    Changes,
    #[serde(alias = "merged")]
    Landed,
    Failed,
    Finished,
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwarmFeedEvent {
    pub timestamp: String,
    pub actor: String,
    #[cfg_attr(feature = "bindings", specta(type = SwarmEventKindBinding))]
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
    /// The member's Rosen change, when its project uses Rosen.
    pub rosen_change: Option<String>,
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
    /// The track's step names, its tooltip.
    pub track_label: String,
    pub track: Vec<TrackStep>,
    pub doing: String,
    pub provider: ProviderColor,
    pub agent_label: String,
    pub added: u32,
    pub deleted: u32,
    /// How many subagents this member's thread currently has running (F4, F10).
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub running_subagents: usize,
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
    /// "landed/total landed" once a member's change has landed.
    pub landed_label: Option<String>,
    pub segments: Vec<SwarmStage>,
    pub legend: Vec<SwarmLegendItem>,
    pub cards: Vec<SwarmCard>,
    pub feed: Vec<SwarmFeedEvent>,
    pub composer_placeholder: String,
}

pub fn member_stage(state: ThreadState, review_stage: Option<ReviewStage>) -> SwarmStage {
    match state {
        ThreadState::Merged | ThreadState::Done => SwarmStage::Landed,
        ThreadState::Error | ThreadState::Closed => SwarmStage::Failed,
        ThreadState::Review if review_stage == Some(ReviewStage::Fixing) => SwarmStage::Fixing,
        ThreadState::Review => SwarmStage::Named,
        ThreadState::Draft | ThreadState::Spec | ThreadState::Working => SwarmStage::Working,
    }
}

/// The Rosen track's step names: the agent edits, names its change, and Rosen lands it.
pub const ROSEN_TRACK_LABEL: &str = "editing · named · landed";
/// The legacy git track's step names, for members of projects still on git.
pub const GIT_TRACK_LABEL: &str = "work · change · CI · review · landed";

/// editing · named · landed for a Rosen member: a refused land or a revert reopens it.
pub fn rosen_member_track(stage: SwarmStage) -> Vec<TrackStep> {
    use TrackStep::{Done, Failed, Running, Todo};
    match stage {
        SwarmStage::Choosing | SwarmStage::Working => vec![Running, Todo, Todo],
        SwarmStage::Named => vec![Done, Running, Todo],
        SwarmStage::Fixing => vec![Running, Failed, Todo],
        SwarmStage::Landed => vec![Done, Done, Done],
        SwarmStage::Failed => vec![Failed, Todo, Todo],
    }
}

/// The member's track: Rosen's lifecycle, or the legacy git change's checks and review.
pub fn member_track(member: &MemberProgress) -> Vec<TrackStep> {
    use TrackStep::{Done, Failed, Running, Todo};
    if member.rosen_change.is_some() {
        return rosen_member_track(member_stage(member.state, member.review_stage));
    }
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
    let stage = member_stage(member.state, member.review_stage);
    if let Some(change) = &member.rosen_change {
        return match stage {
            SwarmStage::Choosing | SwarmStage::Working => member.activity_label.clone(),
            SwarmStage::Named => format!("named · {change}"),
            SwarmStage::Fixing => format!("reopened · {change}"),
            SwarmStage::Landed => format!("landed · {change}"),
            SwarmStage::Failed => "failed · workspace kept".into(),
        };
    }
    let number = member.pull_request_number.unwrap_or_default();
    match stage {
        SwarmStage::Choosing | SwarmStage::Working => member.activity_label.clone(),
        SwarmStage::Landed => format!("landed · change #{number}"),
        SwarmStage::Failed if member.state == ThreadState::Closed => {
            format!("change #{number} closed · workspace kept")
        }
        SwarmStage::Failed => "failed · workspace kept".into(),
        SwarmStage::Fixing => format!("change #{number} · addressing review"),
        SwarmStage::Named => match member.review_stage {
            Some(ReviewStage::ContinuousIntegration) => format!("change #{number} · waiting on CI"),
            Some(ReviewStage::Approved) => format!("change #{number} · approved · landing"),
            _ => format!("change #{number} · in review"),
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
        track_label: if member.rosen_change.is_some() {
            ROSEN_TRACK_LABEL.into()
        } else {
            GIT_TRACK_LABEL.into()
        },
        track: member_track(member),
        doing: member_doing(member),
        provider: member.provider,
        agent_label: format!("a{} · {}", member.agent_number, member.model_name),
        added: member.added,
        deleted: member.deleted,
        running_subagents: 0,
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

fn landed_count(members: &[MemberProgress]) -> usize {
    segments(members)
        .into_iter()
        .filter(|stage| *stage == SwarmStage::Landed)
        .count()
}

/// A member still at work: choosing, editing, named or reopened. Landed, closed and
/// failed members are finished, and deleted ones are not members at all.
pub fn is_live(stage: SwarmStage) -> bool {
    matches!(
        stage,
        SwarmStage::Choosing | SwarmStage::Working | SwarmStage::Named | SwarmStage::Fixing
    )
}

/// The sidebar row's ×N and meta line from the stages of the members nested under it
/// (finished ones have moved to Settled). Only live members count towards ×N; a swarm with
/// none left reads "finished" (#283).
pub fn row_summary(stages: &[SwarmStage]) -> (usize, String) {
    let live = stages.iter().filter(|stage| is_live(**stage)).count();
    let mut parts: Vec<String> = [
        SwarmStage::Choosing,
        SwarmStage::Working,
        SwarmStage::Named,
        SwarmStage::Fixing,
        SwarmStage::Failed,
    ]
    .into_iter()
    .filter_map(|stage| {
        let members = stages.iter().filter(|item| **item == stage).count();
        (members > 0).then(|| format!("{members} {}", stage.label()))
    })
    .collect();
    if live == 0 {
        parts.insert(0, "finished".to_owned());
    }
    (live, parts.join(" · "))
}

pub fn project_swarm_view(entry: &SwarmEntry) -> SwarmViewSnapshot {
    let total = entry.members.len();
    let landed = landed_count(&entry.members);
    let live = segments(&entry.members).into_iter().any(is_live);
    SwarmViewSnapshot {
        identifier: entry.identifier,
        title: entry.title.clone(),
        agents_label: if live {
            format!(
                "{total} agent{} · {} workspace{} live",
                if total == 1 { "" } else { "s" },
                total - landed,
                if total - landed == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "{total} agent{} · finished",
                if total == 1 { "" } else { "s" }
            )
        },
        started_label: format!("started {}", entry.started_label),
        landed_label: (landed > 0).then(|| format!("{landed}/{total} landed")),
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
