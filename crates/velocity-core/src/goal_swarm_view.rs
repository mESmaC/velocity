//! Goal swarm view and sidebar row read models (SPEC.md F16 Goal swarm view, Sidebar).
//!
//! The store reads each agent's saved thread into [`GoalAgentProgress`]; these pure
//! functions turn that into the segment bar, one card per agent, the claims board and
//! the sidebar line. Members are ordinary threads, so their stages reuse the swarm's.

use crate::airc_modal::{NickTone, nick_tone};
use crate::goal_swarm::GoalFeedEvent;
use crate::layout_view::ProviderColor;
use crate::state::{ReviewStage, ThreadState};
use crate::swarm::{SwarmLegendItem, SwarmStage, TrackStep, member_stage};
use crate::work_pool::WorkKey;

/// How many feed lines the view shows, as the swarm view does.
const FEED_SHOWN: usize = 40;

/// One goal swarm agent's current thread, as the store reads it from `state.json`.
#[derive(Clone, Debug)]
pub struct GoalAgentProgress {
    pub nick: String,
    /// The agent's current thread: its choosing thread, or the piece it claimed.
    pub thread_identifier: usize,
    pub provider: ProviderColor,
    pub model_name: String,
    pub effort_label: String,
    pub fast: bool,
    pub choosing: bool,
    /// The piece it claimed (the thread is titled by it).
    pub title: String,
    pub state: ThreadState,
    pub review_stage: Option<ReviewStage>,
    pub pull_request_number: Option<u32>,
    /// The thread's sidebar line (`thinking`, `executing`, …).
    pub activity_label: String,
    /// "waiting for <nick> · #n · shares <file>" while an AIRC dependency wait holds it.
    pub waiting: Option<String>,
}

impl GoalAgentProgress {
    /// A choosing agent; the store fills in what its saved thread says.
    pub fn new(nick: &str, thread_identifier: usize, provider: ProviderColor) -> Self {
        Self {
            nick: nick.to_owned(),
            thread_identifier,
            provider,
            model_name: String::new(),
            effort_label: String::new(),
            fast: false,
            choosing: true,
            title: String::new(),
            state: ThreadState::Working,
            review_stage: None,
            pull_request_number: None,
            activity_label: String::new(),
            waiting: None,
        }
    }

    fn stage(&self) -> SwarmStage {
        if self.choosing && self.state == ThreadState::Working {
            SwarmStage::Choosing
        } else {
            member_stage(self.state, self.review_stage)
        }
    }

    fn waits(&self) -> bool {
        self.waiting.is_some() && self.state == ThreadState::Working
    }

    fn model_label(&self) -> String {
        let fast = if self.fast { " · Fast" } else { "" };
        format!("{} · {}{fast}", self.model_name, self.effort_label)
    }
}

/// A goal swarm's sidebar row and view, as the store reads it.
#[derive(Clone, Debug)]
pub struct GoalSwarmEntry {
    pub identifier: usize,
    pub title: String,
    pub prompt: String,
    pub started_label: String,
    pub keep_going: bool,
    /// One per agent that still has a thread, in launch order.
    pub agents: Vec<GoalAgentProgress>,
    /// PRs the swarm's agents merged, counting earlier rounds of keep going.
    pub merged_pull_requests: usize,
    pub feed: Vec<GoalFeedEvent>,
}

impl GoalSwarmEntry {
    pub fn new(identifier: usize, prompt: &str, started_label: &str, keep_going: bool) -> Self {
        Self {
            identifier,
            title: crate::goal_swarm::goal_swarm_title(prompt),
            prompt: prompt.to_owned(),
            started_label: started_label.to_owned(),
            keep_going,
            agents: Vec::new(),
            merged_pull_requests: 0,
            feed: Vec::new(),
        }
    }
}

/// One agent's card: nick, model, claimed piece, the claim → PR → CI → review → merged
/// track and what it is doing.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct GoalAgentCard {
    /// The thread a click opens.
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub nick: String,
    /// The nick's AIRC colour, so a card matches its lines in the chat.
    pub nick_tone: NickTone,
    pub provider: ProviderColor,
    pub model_label: String,
    pub title: String,
    pub stage: SwarmStage,
    pub track: Vec<TrackStep>,
    pub doing: String,
    pub waiting: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct GoalSwarmViewSnapshot {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub title: String,
    pub agents_label: String,
    pub started_label: String,
    pub merged_label: Option<String>,
    pub prompt: String,
    /// "goal swarm · the same prompt to all <n> · <mix>", under the prompt bubble.
    pub prompt_label: String,
    pub segments: Vec<SwarmStage>,
    pub legend: Vec<SwarmLegendItem>,
    /// "<n> waiting on another agent's PR".
    pub waiting_label: Option<String>,
    pub cards: Vec<GoalAgentCard>,
    pub feed: Vec<GoalFeedEvent>,
    pub composer_placeholder: String,
    /// Changes whenever a claim, PR or merge could have moved the claims board, so the
    /// window reads the board again (`get_goal_swarm_board`).
    pub board_revision: String,
}

/// Segment order in the legend: choosing first, then the swarm lifecycle.
const STAGES: [SwarmStage; 6] = [
    SwarmStage::Choosing,
    SwarmStage::Working,
    SwarmStage::PullRequest,
    SwarmStage::Fixing,
    SwarmStage::Merged,
    SwarmStage::Failed,
];

pub fn goal_segments(agents: &[GoalAgentProgress]) -> Vec<SwarmStage> {
    agents.iter().map(GoalAgentProgress::stage).collect()
}

fn legend(agents: &[GoalAgentProgress]) -> Vec<SwarmLegendItem> {
    let segments = goal_segments(agents);
    STAGES
        .into_iter()
        .filter_map(|stage| {
            let count = segments.iter().filter(|item| **item == stage).count();
            (count > 0).then(|| SwarmLegendItem {
                stage,
                count: u32::try_from(count).unwrap_or(u32::MAX),
                label: format!("{count} {}", stage.label()),
            })
        })
        .collect()
}

/// claim · PR · CI · review · merged, as the reference's `gcCard` lights them.
pub fn goal_track(agent: &GoalAgentProgress) -> Vec<TrackStep> {
    use TrackStep::{Done, Failed, Running, Todo};
    if agent.stage() == SwarmStage::Choosing {
        return vec![Running, Todo, Todo, Todo, Todo];
    }
    let merged = matches!(agent.state, ThreadState::Merged | ThreadState::Done);
    let has_pull_request = agent.pull_request_number.is_some() || merged;
    let stage = agent.review_stage;
    let opened = match agent.state {
        _ if has_pull_request => Done,
        ThreadState::Error | ThreadState::Closed => Failed,
        _ => Running,
    };
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
    vec![Done, opened, checks, review, landed]
}

/// The card's "what it is doing now" line.
pub fn goal_doing(agent: &GoalAgentProgress) -> String {
    if agent.activity_label == "stalled" || agent.activity_label.starts_with("at limit") {
        return agent.activity_label.clone();
    }
    let number = agent.pull_request_number.unwrap_or_default();
    match agent.stage() {
        SwarmStage::Choosing => "reading the claims board".into(),
        SwarmStage::Working => agent
            .waiting
            .clone()
            .unwrap_or_else(|| agent.activity_label.clone()),
        SwarmStage::Merged => format!("merged · PR #{number}"),
        SwarmStage::Failed if agent.state == ThreadState::Closed => {
            format!("PR #{number} closed · claim held")
        }
        SwarmStage::Failed => "failed · claim held".into(),
        SwarmStage::Fixing | SwarmStage::PullRequest => {
            format!("PR #{number} · {}", review_label(agent.review_stage))
        }
    }
}

fn review_label(stage: Option<ReviewStage>) -> &'static str {
    match stage {
        Some(ReviewStage::ContinuousIntegration) => "waiting on CI",
        Some(ReviewStage::Fixing) => "addressing review",
        Some(ReviewStage::Approved) => "approved · merging",
        Some(ReviewStage::Review) | None => "in review",
    }
}

pub fn goal_card(agent: &GoalAgentProgress) -> GoalAgentCard {
    GoalAgentCard {
        identifier: agent.thread_identifier,
        nick: agent.nick.clone(),
        nick_tone: nick_tone(&agent.nick),
        provider: agent.provider,
        model_label: agent.model_label(),
        title: if agent.stage() == SwarmStage::Choosing {
            "choosing work…".into()
        } else {
            agent.title.clone()
        },
        stage: agent.stage(),
        track: goal_track(agent),
        doing: goal_doing(agent),
        waiting: agent.waits(),
    }
}

/// "6 GPT-6.1 Sol High · 2 Fable 5.1 High · Fast": agents per model, effort and speed,
/// in launch order, so + Agents shows in the mix too.
pub fn mix_label(agents: &[GoalAgentProgress]) -> String {
    let mut groups: Vec<(String, usize)> = Vec::new();
    for agent in agents {
        let fast = if agent.fast { " · Fast" } else { "" };
        let key = format!("{} {}{fast}", agent.model_name, agent.effort_label);
        match groups.iter_mut().find(|(name, _)| *name == key) {
            Some((_, count)) => *count += 1,
            None => groups.push((key, 1)),
        }
    }
    groups
        .into_iter()
        .map(|(key, count)| format!("{count} {key}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The sidebar row's meta line: "2 choosing · 3 working · 1 PR open · 1/6 merged".
pub fn goal_sidebar_summary(agents: &[GoalAgentProgress]) -> String {
    if agents.is_empty() {
        return "no agents".into();
    }
    let merged = goal_segments(agents)
        .into_iter()
        .filter(|stage| *stage == SwarmStage::Merged)
        .count();
    if merged == agents.len() {
        return format!("all {merged} merged");
    }
    let mut parts: Vec<String> = legend(agents)
        .into_iter()
        .filter(|item| item.stage != SwarmStage::Merged)
        .map(|item| item.label)
        .collect();
    parts.push(format!("{merged}/{} merged", agents.len()));
    parts.join(" · ")
}

pub fn project_goal_swarm_view(entry: &GoalSwarmEntry) -> GoalSwarmViewSnapshot {
    let total = entry.agents.len();
    let waits = entry.agents.iter().filter(|agent| agent.waits()).count();
    let merged = entry.merged_pull_requests;
    let keep_going = if entry.keep_going {
        " · keep going"
    } else {
        ""
    };
    GoalSwarmViewSnapshot {
        identifier: entry.identifier,
        title: entry.title.clone(),
        agents_label: format!("{total} agent{} · one prompt", plural(total)),
        started_label: format!("started {}", entry.started_label),
        merged_label: (merged > 0).then(|| format!("{merged} PR{} merged", plural(merged))),
        prompt: entry.prompt.clone(),
        prompt_label: format!(
            "goal swarm · the same prompt to all {total} · {}{keep_going}",
            mix_label(&entry.agents)
        ),
        segments: goal_segments(&entry.agents),
        legend: legend(&entry.agents),
        waiting_label: (waits > 0).then(|| format!("{waits} waiting on another agent's PR")),
        cards: entry.agents.iter().map(goal_card).collect(),
        feed: entry
            .feed
            .iter()
            .rev()
            .take(FEED_SHOWN)
            .rev()
            .cloned()
            .collect(),
        composer_placeholder: "Message every agent in this goal swarm, e.g. \u{201c}leave the composer alone until #42 merges\u{201d}".into(),
        board_revision: board_revision(entry),
    }
}

fn board_revision(entry: &GoalSwarmEntry) -> String {
    let agents: Vec<String> = entry
        .agents
        .iter()
        .map(|agent| {
            format!(
                "{}:{:?}:{:?}:{}",
                agent.thread_identifier,
                agent.stage(),
                agent.pull_request_number,
                agent.waits()
            )
        })
        .collect();
    format!("{}|{}", entry.feed.len(), agents.join(","))
}

/// Who holds a claimed piece, read from the holder's saved thread.
#[derive(Clone, Debug)]
pub struct ClaimHolder {
    pub nick: String,
    pub state: ThreadState,
    pub review_stage: Option<ReviewStage>,
    pub pull_request_number: Option<u32>,
    pub waiting: Option<String>,
}

impl ClaimHolder {
    pub fn new(nick: &str, state: ThreadState) -> Self {
        Self {
            nick: nick.to_owned(),
            state,
            review_stage: None,
            pull_request_number: None,
            waiting: None,
        }
    }
}

/// One piece of the work pool, claimed or open.
#[derive(Clone, Debug)]
pub struct BoardPiece {
    pub key: WorkKey,
    pub title: String,
    pub holder: Option<ClaimHolder>,
}

impl BoardPiece {
    pub fn new(key: WorkKey, title: &str, holder: Option<ClaimHolder>) -> Self {
        Self {
            key,
            title: title.to_owned(),
            holder,
        }
    }
}

/// A piece this swarm merged: struck through at the end of the board.
#[derive(Clone, Debug)]
pub struct MergedPiece {
    pub key: Option<WorkKey>,
    pub title: String,
    pub nick: String,
    pub pull_request_number: Option<u32>,
}

impl MergedPiece {
    pub fn new(key: Option<WorkKey>, title: &str, nick: &str, number: Option<u32>) -> Self {
        Self {
            key,
            title: title.to_owned(),
            nick: nick.to_owned(),
            pull_request_number: number,
        }
    }
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", serde(rename_all = "snake_case"))]
pub enum BoardPieceKind {
    Task,
    Issue,
    Spec,
}

/// How a board row is styled: the holder's state, open, or merged.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", serde(rename_all = "snake_case"))]
pub enum BoardRowState {
    Claimed,
    Waiting,
    Failed,
    Open,
    Merged,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct GoalBoardRow {
    pub kind: BoardPieceKind,
    pub title: String,
    pub reference: String,
    /// The holder's nick, or "open".
    pub holder: String,
    /// The holder's AIRC colour; `None` for an open piece.
    pub holder_tone: Option<NickTone>,
    pub status: String,
    pub state: BoardRowState,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct GoalClaimsBoard {
    /// "Claims · 4 claimed · 9 open · claims are locked, the first one wins".
    pub heading: String,
    pub rows: Vec<GoalBoardRow>,
}

/// The claims board (F16): claimed pieces with holder and state, then open pieces, then
/// the pieces this swarm merged, struck through with "merged #n".
pub fn claims_board(pieces: &[BoardPiece], merged: &[MergedPiece]) -> GoalClaimsBoard {
    let landed = |piece: &&BoardPiece| {
        piece
            .holder
            .as_ref()
            .is_some_and(|holder| matches!(holder.state, ThreadState::Merged | ThreadState::Done))
    };
    let claimed: Vec<GoalBoardRow> = pieces
        .iter()
        .filter(|piece| piece.holder.is_some() && !landed(piece))
        .map(claimed_row)
        .collect();
    let open: Vec<GoalBoardRow> = pieces
        .iter()
        .filter(|piece| piece.holder.is_none())
        .map(|piece| GoalBoardRow {
            kind: kind_of(&piece.key),
            title: piece.title.clone(),
            reference: reference_of(&piece.key),
            holder: "open".into(),
            holder_tone: None,
            status: String::new(),
            state: BoardRowState::Open,
        })
        .collect();
    let heading = format!(
        "Claims · {} claimed · {} open · claims are locked, the first one wins",
        claimed.len(),
        open.len()
    );
    let mut rows = claimed;
    rows.extend(open);
    rows.extend(merged.iter().map(merged_row));
    GoalClaimsBoard { heading, rows }
}

fn claimed_row(piece: &BoardPiece) -> GoalBoardRow {
    let (holder, status, state) = match &piece.holder {
        Some(holder) => {
            let (status, state) = holder_status(holder);
            (holder.nick.clone(), status, state)
        }
        None => ("open".into(), String::new(), BoardRowState::Open),
    };
    GoalBoardRow {
        kind: kind_of(&piece.key),
        title: piece.title.clone(),
        reference: reference_of(&piece.key),
        holder_tone: piece.holder.as_ref().map(|holder| nick_tone(&holder.nick)),
        holder,
        status,
        state,
    }
}

fn holder_status(holder: &ClaimHolder) -> (String, BoardRowState) {
    let number = holder.pull_request_number.unwrap_or_default();
    match holder.state {
        ThreadState::Review => (
            format!("#{number} · {}", review_label(holder.review_stage)),
            BoardRowState::Claimed,
        ),
        ThreadState::Error => ("failed · claim held".into(), BoardRowState::Failed),
        ThreadState::Closed => (
            format!("#{number} closed · claim held"),
            BoardRowState::Failed,
        ),
        _ => match &holder.waiting {
            Some(waiting) => (waiting.clone(), BoardRowState::Waiting),
            None => ("working".into(), BoardRowState::Claimed),
        },
    }
}

fn merged_row(piece: &MergedPiece) -> GoalBoardRow {
    GoalBoardRow {
        kind: piece.key.as_ref().map_or(BoardPieceKind::Spec, kind_of),
        title: piece.title.clone(),
        reference: piece.key.as_ref().map(reference_of).unwrap_or_default(),
        holder: piece.nick.clone(),
        holder_tone: Some(nick_tone(&piece.nick)),
        status: piece
            .pull_request_number
            .map_or_else(|| "merged".into(), |number| format!("merged #{number}")),
        state: BoardRowState::Merged,
    }
}

fn kind_of(key: &WorkKey) -> BoardPieceKind {
    match key {
        WorkKey::Task { .. } => BoardPieceKind::Task,
        WorkKey::Issue { .. } => BoardPieceKind::Issue,
        WorkKey::Spec { .. } => BoardPieceKind::Spec,
    }
}

fn reference_of(key: &WorkKey) -> String {
    match key {
        WorkKey::Task { .. } => "task".into(),
        WorkKey::Issue { number, .. } => format!("#{number}"),
        WorkKey::Spec { .. } => "SPEC.md".into(),
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
