//! Read the active project's persisted context into the shell's public read model.

use crate::{
    AgentActivity, PersistedState, ProjectItem, ReviewStage, TaskState, ThreadItem, ThreadState,
};
use std::collections::BTreeSet;
use velocity_core::layout::{LayoutRow, ThreadGroup};
use velocity_core::layout_view::{
    LayoutData, LayoutIntent, PaletteItem, PaletteTarget, RailItem, ThreadEntry,
};
use velocity_core::navigation::NavigationView;

pub(crate) fn active_project(state: &PersistedState) -> Option<&ProjectItem> {
    usize::try_from(state.active_project_index)
        .ok()
        .and_then(|index| state.projects.get(index))
}

pub(crate) fn read_model(state: &PersistedState) -> LayoutData {
    let mut data = LayoutData {
        projects: project_palette(state),
        ..LayoutData::default()
    };
    if let Some(project) = active_project(state) {
        data.project_name = project.name.clone();
        data.threads = project.threads.iter().map(thread_entry).collect();
        data.tasks = project
            .tasks
            .iter()
            .map(|task| RailItem {
                identifier: task.id as usize,
                title: task.text.clone(),
                metadata: task_label(task.state).into(),
            })
            .collect();
        data.pull_requests = pull_requests(project);
        data.merged_today = project.merged_today as usize;
        data.worktree_count = project
            .threads
            .iter()
            .filter(|thread| thread.worktree_path.is_some())
            .count();
        data.worktree_drive = worktree_drive(project, state);
    }
    data
}

fn project_palette(state: &PersistedState) -> Vec<PaletteItem> {
    state
        .projects
        .iter()
        .enumerate()
        .map(|(index, project)| PaletteItem {
            identifier: format!("project:{index}"),
            label: project.name.clone(),
            group: "project".into(),
            shortcut: String::new(),
            target: PaletteTarget::Intent(LayoutIntent::SwitchProject(project.id.to_string())),
        })
        .collect()
}

fn thread_entry(thread: &ThreadItem) -> ThreadEntry {
    ThreadEntry {
        policy: LayoutRow {
            identifier: thread.id as usize,
            group: thread_group(thread.state),
            hidden: false,
            matches_search: true,
            member_of: None,
            expanded: false,
            agent_count: 1,
            paused: false,
        },
        title: thread.title.clone(),
        metadata: thread_metadata(thread),
        view: if thread.state == ThreadState::Spec {
            NavigationView::SpecScan
        } else {
            NavigationView::Thread
        },
    }
}

fn thread_group(state: ThreadState) -> ThreadGroup {
    match state {
        ThreadState::Draft => ThreadGroup::Draft,
        ThreadState::Spec => ThreadGroup::Spec,
        ThreadState::Working => ThreadGroup::Working,
        ThreadState::Review => ThreadGroup::Review,
        ThreadState::Error => ThreadGroup::Error,
        ThreadState::Closed => ThreadGroup::Closed,
        ThreadState::Done => ThreadGroup::Done,
        ThreadState::Merged => ThreadGroup::Merged,
    }
}

fn thread_metadata(thread: &ThreadItem) -> String {
    if thread.state == ThreadState::Review {
        let stage = match thread.review_stage {
            Some(ReviewStage::ContinuousIntegration) => "waiting on CI",
            Some(ReviewStage::Fixing) => "addressing review",
            Some(ReviewStage::Approved) => "approved · merging",
            _ => "in review",
        };
        return thread
            .pull_request_number
            .map_or_else(|| stage.into(), |number| format!("#{number} · {stage}"));
    }
    match thread.activity {
        AgentActivity::Thinking => "thinking",
        AgentActivity::Executing => "executing",
        AgentActivity::Parked => "parked",
        AgentActivity::Stalled => "stalled",
        AgentActivity::AtLimit => "at limit",
        AgentActivity::Idle => "idle",
    }
    .into()
}

fn task_label(state: TaskState) -> &'static str {
    match state {
        TaskState::Draft => "draft",
        TaskState::Todo => "todo",
        TaskState::Now => "agent working",
        TaskState::Done => "done",
    }
}

fn pull_requests(project: &ProjectItem) -> Vec<RailItem> {
    let mut seen = BTreeSet::new();
    project
        .threads
        .iter()
        .filter_map(|thread| {
            let number = thread.pull_request_number?;
            if thread.state != ThreadState::Review || !seen.insert(number) {
                return None;
            }
            Some(RailItem {
                identifier: number as usize,
                title: thread.title.clone(),
                metadata: thread_metadata(thread),
            })
        })
        .collect()
}

fn drive_label(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    if path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic() {
        path[..2].to_owned()
    } else {
        String::new()
    }
}

fn worktree_drive(project: &ProjectItem, state: &PersistedState) -> String {
    let drives: BTreeSet<_> = project
        .threads
        .iter()
        .filter_map(|thread| thread.worktree_path.as_deref())
        .map(drive_label)
        .collect();
    if drives.is_empty() {
        state
            .settings
            .worktrees
            .location_root
            .as_deref()
            .map(drive_label)
            .unwrap_or_default()
    } else if drives.len() == 1 {
        drives.into_iter().next().unwrap_or_default()
    } else {
        // Old worktrees stay on their original drives after a settings change.
        // The shell's count-only label avoids attributing all of them to one drive.
        String::new()
    }
}
