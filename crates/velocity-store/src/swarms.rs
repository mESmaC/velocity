//! Persisted swarms (SPEC.md F10). A swarm is a view over ordinary threads: it keeps which
//! threads it started, its options and its activity feed. Each member is a normal thread
//! with the normal lifecycle, so deleting one returns its task or frees its issue (F12).

use serde::{Deserialize, Serialize};
use specta::Type;
use velocity_core::swarm::{SwarmEventKind, SwarmFeedEvent, SwarmSource};

use crate::issues::ThreadIssue;
use crate::threads::{NewThread, ThreadError};
use crate::{ProjectItem, ProviderKind, StateStore, TaskState, ThreadState};

/// Feed entries kept per swarm; the view shows the last 40.
const FEED_LIMIT: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SwarmMember {
    pub thread: u32,
    /// The card's `a<n>` agent id, counted per swarm.
    pub agent_number: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SwarmRecord {
    pub id: u32,
    pub source: SwarmSource,
    /// `HH:MM` the swarm launched.
    pub started: String,
    /// Close each issue when its PR merges (default on).
    #[serde(default = "enabled")]
    pub close_issues: bool,
    /// Rebase members whose files a merged member PR touched (F6).
    #[serde(default = "enabled")]
    pub rebase_on_merge: bool,
    #[serde(default)]
    pub members: Vec<SwarmMember>,
    #[serde(default)]
    pub feed: Vec<SwarmFeedEvent>,
}

fn enabled() -> bool {
    true
}

/// One open item a swarm member works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Type)]
#[serde(tag = "kind", content = "number", rename_all = "snake_case")]
pub enum SwarmItem {
    Task(u32),
    Issue(u32),
}

/// Launch, or + Items into an existing swarm (`into`), from the Swarm dialog (F10).
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct LaunchSwarmRequest {
    pub project_identifier: u32,
    pub source: SwarmSource,
    pub items: Vec<SwarmItem>,
    /// Providers assigned round-robin, in item order.
    pub providers: Vec<ProviderKind>,
    pub close_issues: bool,
    pub rebase_on_merge: bool,
    /// Start PR duty, or add the agents it is short of (one reviewer per 3 open PRs).
    pub add_pr_duty: bool,
    pub into: Option<u32>,
}

impl SwarmRecord {
    fn new(id: u32, source: SwarmSource, started: String, request: &LaunchSwarmRequest) -> Self {
        Self {
            id,
            source,
            started,
            close_issues: request.close_issues,
            rebase_on_merge: request.rebase_on_merge,
            members: Vec::new(),
            feed: Vec::new(),
        }
    }

    pub fn title(&self) -> String {
        format!("Swarm · {}", self.source.label())
    }
}

/// The swarm a thread belongs to, and its agent number there.
pub fn thread_swarm(project: &ProjectItem, thread: u32) -> Option<(&SwarmRecord, u32)> {
    project.swarms.iter().find_map(|swarm| {
        swarm
            .members
            .iter()
            .find(|member| member.thread == thread)
            .map(|member| (swarm, member.agent_number))
    })
}

/// Members whose threads still exist, in launch order.
pub fn live_members<'a>(
    project: &'a ProjectItem,
    swarm: &'a SwarmRecord,
) -> impl Iterator<Item = (&'a SwarmMember, &'a crate::ThreadItem)> {
    swarm.members.iter().filter_map(|member| {
        project
            .threads
            .iter()
            .find(|thread| thread.id == member.thread)
            .map(|thread| (member, thread))
    })
}

/// Every member has merged: "Swarm finished · all N PRs merged".
pub fn swarm_finished(project: &ProjectItem, swarm: &SwarmRecord) -> bool {
    let mut members = live_members(project, swarm).peekable();
    members.peek().is_some()
        && members.all(|(_, thread)| matches!(thread.state, ThreadState::Merged))
}

impl StateStore {
    /// Records a new swarm on the active project, or returns `into` when + Items adds to one.
    pub fn open_swarm(
        &mut self,
        request: &LaunchSwarmRequest,
        timestamp: String,
    ) -> Result<u32, ThreadError> {
        let project = request.project_identifier;
        if let Some(existing) = request.into {
            return self
                .project(project)
                .filter(|item| item.swarms.iter().any(|swarm| swarm.id == existing))
                .map(|_| existing)
                .ok_or(ThreadError::UnknownThread {
                    project,
                    thread: existing,
                });
        }
        let mut next = self.state.clone();
        let identifier = crate::threads::next_entry_identifier(&mut next, project)?;
        let item = project_mut(&mut next, project)?;
        let record = SwarmRecord::new(identifier, request.source, timestamp, request);
        item.swarms.insert(0, record);
        item.active_thread_id = Some(identifier);
        self.commit(next)?;
        Ok(identifier)
    }

    /// Records one member thread in a single write: the thread, its swarm membership, and
    /// its task moving to `now` or its issue ownership (F8, F9).
    pub fn add_swarm_member(
        &mut self,
        project: u32,
        swarm: u32,
        thread: NewThread,
        item: SwarmItem,
        issue: Option<ThreadIssue>,
    ) -> Result<u32, ThreadError> {
        let mut next = self.state.clone();
        let identifier = crate::threads::insert_thread(&mut next, project, thread, issue)?;
        let record = project_mut(&mut next, project)?;
        if let SwarmItem::Task(task) = item {
            claim_task(record, task, identifier)?;
        }
        let entry = record
            .swarms
            .iter_mut()
            .find(|entry| entry.id == swarm)
            .ok_or(ThreadError::UnknownThread {
                project,
                thread: swarm,
            })?;
        let agent_number = entry
            .members
            .iter()
            .map(|member| member.agent_number)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        entry.members.push(SwarmMember {
            thread: identifier,
            agent_number,
        });
        // The swarm stays selected while its members start.
        record.active_thread_id = Some(swarm);
        self.commit(next)?;
        Ok(identifier)
    }

    /// Appends to a swarm's activity feed. `false` when the swarm no longer exists.
    pub fn record_swarm_event(
        &mut self,
        project: u32,
        swarm: u32,
        event: SwarmFeedEvent,
    ) -> Result<bool, ThreadError> {
        let mut next = self.state.clone();
        let Some(entry) = project_mut(&mut next, project)?
            .swarms
            .iter_mut()
            .find(|entry| entry.id == swarm)
        else {
            return Ok(false);
        };
        entry.feed.push(event);
        let overflow = entry.feed.len().saturating_sub(FEED_LIMIT);
        entry.feed.drain(..overflow);
        self.commit(next)?;
        Ok(true)
    }

    /// Records a swarm event in the feed of the swarm `thread` belongs to, if any.
    pub fn record_member_event(
        &mut self,
        project: u32,
        thread: u32,
        kind: SwarmEventKind,
        message: String,
        timestamp: &str,
    ) -> Result<Option<u32>, ThreadError> {
        let Some((swarm, agent)) = self
            .project(project)
            .and_then(|item| thread_swarm(item, thread))
            .map(|(swarm, agent)| (swarm.id, agent))
        else {
            return Ok(None);
        };
        let event = SwarmFeedEvent::new(timestamp, &format!("a{agent}"), kind, message);
        self.record_swarm_event(project, swarm, event)?;
        Ok(Some(swarm))
    }
}

fn project_mut(
    state: &mut crate::PersistedState,
    project: u32,
) -> Result<&mut ProjectItem, ThreadError> {
    state
        .projects
        .iter_mut()
        .find(|item| item.id == project)
        .ok_or(ThreadError::UnknownProject(project))
}

/// A task taken by a swarm member is `now`, and refuses a second agent (F10 Items).
fn claim_task(project: &mut ProjectItem, task: u32, thread: u32) -> Result<(), ThreadError> {
    let entry = project
        .tasks
        .iter_mut()
        .find(|entry| entry.id == task)
        .ok_or(ThreadError::UnknownTask(task))?;
    if entry.state != TaskState::Todo || entry.assigned_thread_id.is_some() {
        return Err(ThreadError::TaskUnavailable(task));
    }
    entry.state = TaskState::Now;
    entry.assigned_thread_id = Some(thread);
    if let Some(record) = project.threads.iter_mut().find(|item| item.id == thread) {
        record.task_ids.push(task);
    }
    Ok(())
}

/// The read models the sidebar row and swarm view are built from (F10).
pub(crate) fn swarm_entries(project: &ProjectItem) -> Vec<velocity_core::swarm::SwarmEntry> {
    project
        .swarms
        .iter()
        .map(|swarm| velocity_core::swarm::SwarmEntry {
            identifier: swarm.id as usize,
            source: swarm.source,
            title: swarm.title(),
            started_label: swarm.started.clone(),
            members: live_members(project, swarm)
                .map(|(member, thread)| member_progress(project, member, thread))
                .collect(),
            feed: swarm.feed.clone(),
        })
        .collect()
}

fn member_progress(
    project: &ProjectItem,
    member: &SwarmMember,
    thread: &crate::ThreadItem,
) -> velocity_core::swarm::MemberProgress {
    let (reference, issue) = match &thread.issue {
        Some(issue) => (format!("#{}", issue.number), true),
        None => ("task".to_owned(), false),
    };
    // A member working on a task shows the task's own words, as the reference's cards do.
    let title = thread
        .task_ids
        .first()
        .and_then(|task| project.tasks.iter().find(|entry| entry.id == *task))
        .map_or_else(|| thread.title.clone(), |task| task.text.clone());
    velocity_core::swarm::MemberProgress {
        identifier: thread.id as usize,
        title,
        reference,
        issue,
        agent_number: member.agent_number,
        provider: crate::project_layout::provider_color(thread.provider),
        model_name: thread.model_name.clone(),
        state: thread.state.into(),
        review_stage: thread.review_stage.map(Into::into),
        pull_request_number: thread.pull_request_number,
        activity_label: crate::project_layout::thread_metadata(thread),
        added: thread.additions,
        deleted: thread.deletions,
    }
}

/// The swarm's row under Swarms: ×N, the segment bar and its counts (F10).
pub(crate) fn swarm_row(
    entry: &velocity_core::swarm::SwarmEntry,
) -> velocity_core::layout_view::ThreadEntry {
    use velocity_core::layout::{LayoutRow, ThreadGroup};
    use velocity_core::state::ThreadState as CoreThreadState;
    let members: Vec<_> = entry
        .members
        .iter()
        .filter(|member| {
            !matches!(
                member.state,
                CoreThreadState::Merged | CoreThreadState::Done | CoreThreadState::Closed
            )
        })
        .cloned()
        .collect();
    velocity_core::layout_view::ThreadEntry {
        policy: LayoutRow {
            identifier: entry.identifier,
            group: ThreadGroup::Swarm,
            hidden: false,
            matches_search: true,
            member_of: None,
            expanded: false,
            agent_count: members.len(),
            paused: false,
        },
        title: entry.title.clone(),
        metadata: velocity_core::swarm::sidebar_summary(&members),
        view: velocity_core::navigation::NavigationView::Swarm,
        detail: None,
        duty: None,
    }
}
