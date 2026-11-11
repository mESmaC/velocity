//! The Board view (SPEC.md F1, "Board"): every piece of work as a card in lanes that read
//! left to right, from work nobody has started to work that merged.
//!
//! It is a second way to look at what the sidebar and the rail already show, so the lanes
//! are a pure mapping from those lists. Nothing here holds state.

use crate::layout::ThreadGroup;
use crate::layout_view::{RailItem, SidebarGroup, SidebarRow, TaskView};

/// One lane of the board, in pipeline order.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardLaneKind {
    /// Tasks and issues no thread works on yet.
    UpNext,
    /// Threads with an agent at work, and the ones that failed.
    Running,
    /// Threads whose PR is open.
    InReview,
    /// Threads that finished: merged, done, or closed without merging.
    Finished,
    /// Duty agents. They are not work items, so they get their own lane at the end.
    OnDuty,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardCardKind {
    Task,
    Issue,
    Thread,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct BoardCard {
    pub kind: BoardCardKind,
    /// The thread a click opens. A task or an issue has none.
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number::optional"))]
    #[cfg_attr(feature = "bindings", specta(type = Option<String>))]
    pub thread: Option<usize>,
    pub title: String,
    pub metadata: String,
    /// The thread's sidebar group, which sets the card's state colour.
    pub group: Option<ThreadGroup>,
    /// The thread is the selected one.
    pub active: bool,
    pub paused: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct BoardLane {
    pub kind: BoardLaneKind,
    pub title: String,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub count: usize,
    pub cards: Vec<BoardCard>,
}

/// The four pipeline lanes, always present so the pipeline keeps its shape when a lane is
/// empty, then the duty lane when any duty agent exists.
pub fn board_lanes(
    groups: &[SidebarGroup],
    tasks: &[TaskView],
    issues: &[RailItem],
) -> Vec<BoardLane> {
    let mut lanes = vec![
        lane(
            BoardLaneKind::UpNext,
            "Up next",
            up_next_cards(tasks, issues),
        ),
        lane(
            BoardLaneKind::Running,
            "Running",
            thread_cards(groups, BoardLaneKind::Running),
        ),
        lane(
            BoardLaneKind::InReview,
            "In review",
            thread_cards(groups, BoardLaneKind::InReview),
        ),
        lane(
            BoardLaneKind::Finished,
            "Finished",
            thread_cards(groups, BoardLaneKind::Finished),
        ),
    ];
    let duty = thread_cards(groups, BoardLaneKind::OnDuty);
    if !duty.is_empty() {
        lanes.push(lane(BoardLaneKind::OnDuty, "On duty", duty));
    }
    lanes
}

fn lane(kind: BoardLaneKind, title: &str, cards: Vec<BoardCard>) -> BoardLane {
    BoardLane {
        kind,
        title: title.into(),
        count: cards.len(),
        cards,
    }
}

/// Which lane a thread in `group` belongs to. A draft is the composer, not a card.
fn lane_of(group: ThreadGroup) -> Option<BoardLaneKind> {
    match group {
        ThreadGroup::Draft => None,
        ThreadGroup::Duty => Some(BoardLaneKind::OnDuty),
        ThreadGroup::Swarm | ThreadGroup::Spec | ThreadGroup::Working | ThreadGroup::Error => {
            Some(BoardLaneKind::Running)
        }
        ThreadGroup::Review => Some(BoardLaneKind::InReview),
        ThreadGroup::Closed | ThreadGroup::Done | ThreadGroup::Merged => {
            Some(BoardLaneKind::Finished)
        }
    }
}

fn thread_cards(groups: &[SidebarGroup], kind: BoardLaneKind) -> Vec<BoardCard> {
    groups
        .iter()
        .filter(|group| lane_of(group.group) == Some(kind))
        .flat_map(|group| group.rows.iter().map(thread_card))
        .collect()
}

fn thread_card(row: &SidebarRow) -> BoardCard {
    BoardCard {
        kind: BoardCardKind::Thread,
        thread: Some(row.identifier),
        title: row.title.clone(),
        metadata: row.metadata.clone(),
        group: Some(row.group),
        active: row.active,
        paused: row.paused,
    }
}

/// Tasks nobody has started, then open issues. A task in the `now` state already has a
/// thread, which shows in Running.
fn up_next_cards(tasks: &[TaskView], issues: &[RailItem]) -> Vec<BoardCard> {
    let waiting_tasks = tasks
        .iter()
        .filter(|task| task.state == "todo" || task.state == "draft")
        .map(|task| work_card(BoardCardKind::Task, &task.title, &task.metadata));
    let open_issues = issues
        .iter()
        .map(|issue| work_card(BoardCardKind::Issue, &issue.title, &issue.metadata));
    waiting_tasks.chain(open_issues).collect()
}

fn work_card(kind: BoardCardKind, title: &str, metadata: &str) -> BoardCard {
    BoardCard {
        kind,
        thread: None,
        title: title.into(),
        metadata: metadata.into(),
        group: None,
        active: false,
        paused: false,
    }
}
