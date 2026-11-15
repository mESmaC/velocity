//! Goal members follow the same Rosen events as ordinary threads, in the same save.
use velocity_core::goal_swarm::{GoalEventKind, GoalFeedEvent};

use crate::rosen::RosenProgress;
use crate::{ProjectItem, TaskState};

/// A stack handoff retained until the original change's guarded cleanup completes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoalStack {
    #[serde(default)]
    pub parent: String,
    #[serde(default)]
    pub source_change: String,
    #[serde(default)]
    pub source_root: String,
}

impl GoalStack {
    pub fn new(parent: String, source_change: String, source_root: String) -> Self {
        Self {
            parent,
            source_change,
            source_root,
        }
    }
}

pub(crate) fn apply(project: &mut ProjectItem, identifier: u32, progress: RosenProgress) {
    let Some(thread) = project
        .threads
        .iter()
        .find(|thread| thread.id == identifier)
    else {
        return;
    };
    let Some(goal) = thread.goal_member.clone() else {
        return;
    };
    let change = thread.rosen_change.clone().unwrap_or_default();
    let task_ids = thread.task_ids.clone();
    let issue = thread.issue.clone();
    if let Some(issue) = issue {
        record_issue_completion(project, issue, progress);
    }
    if progress == RosenProgress::Reverted {
        // The shared follow-through path reopens SPEC work while preserving other builds.
        // A revert restores ownership even when the nick already took its next round.
        for task in &mut project.tasks {
            if task_ids.contains(&task.id) || task.assigned_thread_id == Some(identifier) {
                task.state = TaskState::Now;
                task.assigned_thread_id = Some(identifier);
            }
        }
    }
    record_activity(project, goal.swarm_id, &goal.nick, &change, progress);
}

fn record_activity(
    project: &mut ProjectItem,
    swarm_id: u32,
    nick: &str,
    change: &str,
    progress: RosenProgress,
) {
    let (kind, action) = match progress {
        RosenProgress::Named => (GoalEventKind::Opened, "named"),
        RosenProgress::Landed => (GoalEventKind::Merged, "landed"),
        RosenProgress::Reverted => (GoalEventKind::Changes, "reverted · fixing"),
        RosenProgress::ChangesRequested => (GoalEventKind::Changes, "fixing"),
    };
    if let Some(swarm) = project
        .goal_swarms
        .iter_mut()
        .find(|swarm| swarm.id == swarm_id)
    {
        let message = format!("{nick} {action} change {change}");
        swarm.feed.push(GoalFeedEvent::new(
            &crate::duty::clock_timestamp(),
            nick,
            kind,
            message,
        ));
        let overflow = swarm.feed.len().saturating_sub(200);
        swarm.feed.drain(..overflow);
    }
}

fn record_issue_completion(
    project: &mut ProjectItem,
    issue: crate::issues::ThreadIssue,
    progress: RosenProgress,
) {
    if progress == RosenProgress::Landed && !project.completed_goal_issues.contains(&issue) {
        project.completed_goal_issues.push(issue);
    } else if progress == RosenProgress::Reverted {
        project
            .completed_goal_issues
            .retain(|completed| completed != &issue);
    }
}
