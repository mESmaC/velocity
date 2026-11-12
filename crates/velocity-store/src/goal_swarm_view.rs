//! What the goal swarm's sidebar row, view and claims board read from saved state
//! (`SPEC.md` F16). The agents are ordinary threads; the record keeps who they are.

use std::collections::BTreeSet;

use velocity_core::goal_swarm_view::{
    BoardPiece, ClaimHolder, GoalAgentProgress, GoalClaimsBoard, GoalSwarmEntry, MergedPiece,
    claims_board, goal_row_summary,
};
use velocity_core::layout_view::ThreadEntry;
use velocity_core::work_pool::{WorkKey, WorkPool};

use crate::goal_swarms::{GoalSwarmMember, GoalSwarmRecord};
use crate::project_layout::{effort_label, provider_color, thread_metadata};
use crate::{ProjectItem, SpeedMode, ThreadItem, ThreadState};

/// Every goal swarm on the project, newest first, as the sidebar and view read them.
pub(crate) fn goal_swarm_entries(project: &ProjectItem) -> Vec<GoalSwarmEntry> {
    project
        .goal_swarms
        .iter()
        .map(|swarm| goal_swarm_entry(project, swarm))
        .collect()
}

fn goal_swarm_entry(project: &ProjectItem, swarm: &GoalSwarmRecord) -> GoalSwarmEntry {
    let mut entry = GoalSwarmEntry::new(
        swarm.id as usize,
        &swarm.prompt,
        &swarm.started,
        swarm.keep_going,
    );
    entry.agents = swarm
        .members
        .iter()
        .filter_map(|member| agent_progress(project, member))
        .collect();
    entry.merged_pull_requests = merged_threads(project, swarm).len();
    entry.feed = swarm.feed.clone();
    entry
}

fn find_thread(project: &ProjectItem, identifier: u32) -> Option<&ThreadItem> {
    project
        .threads
        .iter()
        .find(|thread| thread.id == identifier)
}

/// An agent's card reads its current thread; an agent that left before claiming
/// anything has none and no card.
fn agent_progress(project: &ProjectItem, member: &GoalSwarmMember) -> Option<GoalAgentProgress> {
    let thread = find_thread(project, member.thread?)?;
    let mut agent = GoalAgentProgress::new(
        &member.nick,
        thread.id as usize,
        provider_color(member.provider),
    );
    agent.model_name = member.model_name.clone();
    agent.effort_label = effort_label(member.effort).into();
    agent.fast = member.speed == SpeedMode::Fast;
    agent.choosing = thread
        .goal_member
        .as_ref()
        .is_some_and(|goal| goal.choosing);
    agent.title = thread.title.clone();
    agent.state = thread.state.into();
    agent.review_stage = thread.review_stage.map(Into::into);
    agent.pull_request_number = thread.pull_request_number;
    agent.activity_label = thread_metadata(thread);
    agent.waiting = thread.dependency_wait.clone();
    Some(agent)
}

/// Threads this swarm's agents merged, each once, in launch then merge order.
fn merged_threads<'a>(
    project: &'a ProjectItem,
    swarm: &'a GoalSwarmRecord,
) -> Vec<(&'a ThreadItem, &'a str)> {
    let mut seen = BTreeSet::new();
    let mut merged = Vec::new();
    for member in &swarm.members {
        let rounds = member.merged_threads.iter().chain(member.thread.as_ref());
        for identifier in rounds {
            let Some(thread) = find_thread(project, *identifier) else {
                continue;
            };
            if thread.state == ThreadState::Merged && seen.insert(thread.id) {
                let nick = thread
                    .goal_member
                    .as_ref()
                    .map_or(member.nick.as_str(), |goal| goal.nick.as_str());
                merged.push((thread, nick));
            }
        }
    }
    merged
}

/// Where a goal swarm member nests in the sidebar: under its swarm, by launch order.
pub(crate) fn goal_membership(
    project: &ProjectItem,
    thread: &ThreadItem,
) -> Option<(u32, u32, u32)> {
    let goal = thread.goal_member.as_ref()?;
    let swarm = project
        .goal_swarms
        .iter()
        .find(|swarm| swarm.id == goal.swarm_id)?;
    let order = swarm
        .members
        .iter()
        .position(|member| member.nick == goal.nick)
        .map_or(u32::MAX, |index| u32::try_from(index).unwrap_or(u32::MAX));
    Some((swarm.id, order, thread.id))
}

/// The goal swarm's row under Swarms: target icon, "Goal · <prompt>", ×N, segment bar.
pub(crate) fn goal_swarm_row(entry: &GoalSwarmEntry) -> ThreadEntry {
    let (agents, metadata) = goal_row_summary(&entry.agents);
    ThreadEntry::swarm_row(entry.identifier, entry.title.clone(), metadata, agents)
}

/// The claims board for one goal swarm: every piece in `pool`, held pieces with their
/// holder's nick and state, and the pieces this swarm merged (F16 Goal swarm view).
pub fn goal_claims_board(
    project: &ProjectItem,
    swarm_identifier: u32,
    pool: &WorkPool,
) -> Option<GoalClaimsBoard> {
    let swarm = project
        .goal_swarms
        .iter()
        .find(|swarm| swarm.id == swarm_identifier)?;
    let pieces: Vec<BoardPiece> = pool
        .pieces
        .iter()
        .map(|piece| {
            let holder = piece
                .thread_identifier
                .and_then(|identifier| find_thread(project, identifier))
                .map(claim_holder);
            BoardPiece::new(piece.key.clone(), &piece.title, holder)
        })
        .collect();
    let merged: Vec<MergedPiece> = merged_threads(project, swarm)
        .into_iter()
        .map(|(thread, nick)| {
            MergedPiece::new(
                thread_piece(thread),
                &thread.title,
                nick,
                thread.pull_request_number,
            )
        })
        .collect();
    Some(claims_board(&pieces, &merged))
}

/// Goal agents go by their nick; other threads by the AIRC member name they join as.
fn claim_holder(thread: &ThreadItem) -> ClaimHolder {
    let nick = thread
        .goal_member
        .as_ref()
        .map_or_else(|| format!("thread-{}", thread.id), |goal| goal.nick.clone());
    let mut holder = ClaimHolder::new(&nick, thread.state.into());
    holder.review_stage = thread.review_stage.map(Into::into);
    holder.pull_request_number = thread.pull_request_number;
    holder.waiting = thread.dependency_wait.clone();
    holder
}

fn thread_piece(thread: &ThreadItem) -> Option<WorkKey> {
    if let Some(issue) = &thread.issue {
        return Some(WorkKey::Issue {
            repository: issue.repository.clone(),
            number: issue.number,
        });
    }
    if let Some(task) = thread.task_ids.first() {
        return Some(WorkKey::Task { identifier: *task });
    }
    thread.spec_section.as_ref().map(|section| WorkKey::Spec {
        section: section.clone(),
    })
}
