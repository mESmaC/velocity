//! Saved goal swarms and their members (`SPEC.md` F16).
//!
//! A goal swarm starts N agents with one prompt and the user's model mix. Each member
//! receives a project-scoped nick (`sol-1`, `argon-1`), enters `choosing work`, claims an
//! unclaimed piece in AIRC, and either proceeds through the normal PR lifecycle or leaves
//! if nothing remains. With `keep_going` on, a merged member takes the next open piece.

use serde::{Deserialize, Serialize};
use specta::Type;
use velocity_core::goal_swarm::{
    self, DEFAULT_PROMPT, GoalEventKind, GoalFeedEvent, MAXIMUM_AGENTS,
};
use velocity_core::work_pool::{WorkKey, WorkPiece, WorkSources};

use crate::build_gate::{LaunchRequest, require_launch};
use crate::goal_swarm_dialog::{GoalMixRow, mix_rows_summary, used_nick_numbers};
use crate::issues::{ThreadIssue, issue_thread};
use crate::swarm_dialog::provider_model_name;
use crate::threads::{NewThread, ThreadError};
use crate::{
    EffortLevel, ProjectItem, ProviderKind, Settings, SpeedMode, StateStore, TaskState, ThreadItem,
    ThreadState,
};

const FEED_LIMIT: usize = 200;
pub const CHOOSING_THREAD_TITLE: &str = "Choosing work in AIRC";

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct LaunchGoalSwarmRequest {
    pub project_identifier: u32,
    pub prompt: String,
    #[serde(default)]
    pub sources: WorkSources,
    pub mix: Vec<GoalMixRow>,
    #[serde(default)]
    pub keep_going: bool,
}

impl LaunchGoalSwarmRequest {
    pub fn new(
        project_identifier: u32,
        prompt: String,
        mix: Vec<GoalMixRow>,
        keep_going: bool,
    ) -> Self {
        Self {
            project_identifier,
            prompt,
            sources: WorkSources::default(),
            mix,
            keep_going,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct AddGoalAgentsRequest {
    pub project_identifier: u32,
    pub swarm_identifier: u32,
    pub mix: Vec<GoalMixRow>,
}

impl AddGoalAgentsRequest {
    pub fn new(project_identifier: u32, swarm_identifier: u32, mix: Vec<GoalMixRow>) -> Self {
        Self {
            project_identifier,
            swarm_identifier,
            mix,
        }
    }
}

/// Goal swarm membership stored on a `ThreadItem` (`SPEC.md` F16).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct GoalThreadMember {
    pub swarm_id: u32,
    pub nick: String,
    #[serde(default)]
    pub choosing: bool,
}

impl GoalThreadMember {
    pub fn choosing(swarm_id: u32, nick: String) -> Self {
        Self {
            swarm_id,
            nick,
            choosing: true,
        }
    }
}

/// One agent slot in a saved goal swarm.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct GoalSwarmMember {
    pub nick: String,
    pub provider: ProviderKind,
    pub model_name: String,
    pub effort: EffortLevel,
    pub speed: SpeedMode,
    #[serde(default)]
    pub thread: Option<u32>,
    #[serde(default)]
    pub left_reason: Option<String>,
    #[serde(default, rename = "landed_threads", alias = "merged_threads")]
    pub merged_threads: Vec<u32>,
}

/// Planned launch slot handed to the goal swarm runner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalAgentSlot {
    pub swarm_id: u32,
    pub prompt: String,
    pub sources: WorkSources,
    pub nick: String,
    pub provider: ProviderKind,
    pub model_name: String,
    pub effort: EffortLevel,
    pub speed: SpeedMode,
    pub keep_going: bool,
    pub previous_work_thread: Option<u32>,
}

/// Persisted goal swarm record on `ProjectItem` (`SPEC.md` F16).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct GoalSwarmRecord {
    pub id: u32,
    pub prompt: String,
    #[serde(default)]
    pub sources: WorkSources,
    #[serde(default)]
    pub keep_going: bool,
    pub started: String,
    #[serde(default)]
    pub mix_summary: String,
    #[serde(default)]
    pub members: Vec<GoalSwarmMember>,
    #[serde(default)]
    pub feed: Vec<GoalFeedEvent>,
}

impl GoalAgentSlot {
    /// Restores a choosing member's launch after a failed Rosen workspace setup.
    pub fn for_thread(project: &ProjectItem, thread: &ThreadItem) -> Option<Self> {
        let membership = thread
            .goal_member
            .as_ref()
            .filter(|member| member.choosing)?;
        let swarm = project
            .goal_swarms
            .iter()
            .find(|swarm| swarm.id == membership.swarm_id)?;
        Some(Self {
            swarm_id: swarm.id,
            prompt: swarm.prompt.clone(),
            sources: swarm.sources,
            nick: membership.nick.clone(),
            provider: thread.provider,
            model_name: thread.model_name.clone(),
            effort: thread.effort,
            speed: thread.preferred_speed,
            keep_going: swarm.keep_going,
            previous_work_thread: thread.previous_work_thread,
        })
    }
}

impl GoalSwarmRecord {
    pub fn title(&self) -> String {
        goal_swarm::goal_swarm_title(&self.prompt)
    }
}

pub fn thread_goal_swarm(
    project: &ProjectItem,
    thread_id: u32,
) -> Option<(&GoalSwarmRecord, String)> {
    let member = project
        .threads
        .iter()
        .find(|thread| thread.id == thread_id)
        .and_then(|thread| thread.goal_member.as_ref())?;
    let swarm = project
        .goal_swarms
        .iter()
        .find(|swarm| swarm.id == member.swarm_id)?;
    Some((swarm, member.nick.clone()))
}

impl StateStore {
    /// Opens a new goal swarm and allocates project-scoped nicks for all N agents.
    pub fn open_goal_swarm(
        &mut self,
        request: &LaunchGoalSwarmRequest,
        timestamp: String,
    ) -> Result<(u32, Vec<GoalAgentSlot>), ThreadError> {
        require_goal_gate(&self.state, request.project_identifier)?;
        let rows = validated_rows(&request.mix, &self.state.settings)?;
        if request.sources.is_empty() {
            return Err(ThreadError::InvalidLaunch(
                "Choose at least one work source".into(),
            ));
        }
        let prompt = normalized_prompt(&request.prompt);
        let mut next = self.state.clone();
        let swarm_id =
            crate::threads::next_entry_identifier(&mut next, request.project_identifier)?;
        let settings = next.settings.clone();
        let project = project_mut(&mut next, request.project_identifier)?;
        let (members, slots) = build_slots(
            &settings,
            project,
            swarm_id,
            &prompt,
            request.sources,
            request.keep_going,
            &rows,
        );
        let summary = mix_rows_summary(&settings, &rows);
        let launch_line = format!(
            "Launched {} agent{} on one prompt \u{00b7} {summary}",
            slots.len(),
            if slots.len() == 1 { "" } else { "s" }
        );
        let record = GoalSwarmRecord {
            id: swarm_id,
            prompt,
            sources: request.sources,
            keep_going: request.keep_going,
            started: timestamp.clone(),
            mix_summary: summary,
            members,
            feed: vec![GoalFeedEvent::new(
                &timestamp,
                "you",
                GoalEventKind::You,
                launch_line,
            )],
        };
        project.goal_swarms.insert(0, record);
        self.commit(next)?;
        Ok((swarm_id, slots))
    }

    /// `+ Agents`: appends more agents to an existing goal swarm on its same prompt.
    pub fn add_goal_swarm_agents(
        &mut self,
        request: &AddGoalAgentsRequest,
        timestamp: &str,
    ) -> Result<Vec<GoalAgentSlot>, ThreadError> {
        require_goal_gate(&self.state, request.project_identifier)?;
        let rows = validated_rows(&request.mix, &self.state.settings)?;
        let mut next = self.state.clone();
        let settings = next.settings.clone();
        let project = project_mut(&mut next, request.project_identifier)?;
        let (prompt, sources, keep_going) = project
            .goal_swarms
            .iter()
            .find(|swarm| swarm.id == request.swarm_identifier)
            .map(|swarm| (swarm.prompt.clone(), swarm.sources, swarm.keep_going))
            .ok_or(ThreadError::UnknownThread {
                project: request.project_identifier,
                thread: request.swarm_identifier,
            })?;
        let (members, slots) = build_slots(
            &settings,
            project,
            request.swarm_identifier,
            &prompt,
            sources,
            keep_going,
            &rows,
        );
        let summary = mix_rows_summary(&settings, &rows);
        let swarm = goal_swarm_mut(
            project,
            request.project_identifier,
            request.swarm_identifier,
        )?;
        swarm.members.extend(members);
        push_feed(
            swarm,
            GoalFeedEvent::new(
                timestamp,
                "you",
                GoalEventKind::You,
                format!(
                    "Added {} agent{} \u{00b7} {summary}",
                    slots.len(),
                    if slots.len() == 1 { "" } else { "s" }
                ),
            ),
        );
        self.commit(next)?;
        Ok(slots)
    }

    /// Creates a `choosing work` thread for `slot` and links it to its `GoalSwarmMember`.
    pub fn start_goal_member_thread(
        &mut self,
        project_id: u32,
        slot: &GoalAgentSlot,
        thread: NewThread,
    ) -> Result<u32, ThreadError> {
        let mut next = self.state.clone();
        let selected = project_mut(&mut next, project_id)?.active_thread_id;
        let identifier = crate::threads::insert_thread(&mut next, project_id, thread, None)?;
        let project = project_mut(&mut next, project_id)?;
        if let Some(previous) = slot.previous_work_thread {
            if !project.threads.iter().any(|thread| {
                thread.id == previous
                    && matches!(thread.state, ThreadState::Merged | ThreadState::Done)
            }) {
                return Err(ThreadError::InvalidLaunch(
                    "The previous item reopened".into(),
                ));
            }
            project.active_thread_id = if selected == Some(previous) {
                Some(identifier)
            } else {
                selected
            };
        }
        if let Some(record) = project
            .threads
            .iter_mut()
            .find(|item| item.id == identifier)
        {
            record.goal_member = Some(GoalThreadMember::choosing(slot.swarm_id, slot.nick.clone()));
            record.agent_name = Some(velocity_core::thread_titles::thread_agent_name(
                &slot.nick, identifier,
            ));
            record.previous_work_thread = slot.previous_work_thread;
        }
        let swarm = goal_swarm_mut(project, project_id, slot.swarm_id)?;
        if let Some(member) = swarm.members.iter_mut().find(|item| item.nick == slot.nick) {
            if slot.previous_work_thread.is_some() && member.thread != slot.previous_work_thread {
                return Err(ThreadError::InvalidLaunch(
                    "The agent already took its next item".into(),
                ));
            }
            member.thread = Some(identifier);
            member.left_reason = None;
        }
        self.commit(next)?;
        Ok(identifier)
    }

    /// Transitions a choosing thread to its claimed piece (`choosing -> claimed -> working`).
    pub fn claim_goal_piece(
        &mut self,
        project_id: u32,
        thread_id: u32,
        piece: &WorkPiece,
        branch_name: String,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let project = project_mut(&mut next, project_id)?;
        bind_piece_owner(project, thread_id, piece)?;
        let record = project
            .threads
            .iter_mut()
            .find(|item| item.id == thread_id)
            .ok_or(ThreadError::UnknownThread {
                project: project_id,
                thread: thread_id,
            })?;
        record.title = piece.title.clone();
        record.branch_name = branch_name;
        if let Some(member) = record.goal_member.as_mut() {
            member.choosing = false;
        }
        crate::rosen_work::track_claim(project, thread_id);
        self.commit(next).map_err(ThreadError::from)
    }

    /// Drops an untouched choosing thread when nothing unclaimed remains, recording why the
    /// agent left on its `GoalSwarmMember` (`SPEC.md` F16 step 3).
    pub fn finish_goal_member_leave(
        &mut self,
        project_id: u32,
        swarm_id: u32,
        nick: &str,
        choosing_thread: Option<u32>,
        reason: String,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        {
            let project = project_mut(&mut next, project_id)?;
            let previous = choosing_thread.and_then(|id| {
                project
                    .threads
                    .iter()
                    .find(|thread| thread.id == id)?
                    .previous_work_thread
            });
            if let Some(thread_id) = choosing_thread {
                remove_untouched_choosing_thread(project, thread_id);
            }
            let swarm = goal_swarm_mut(project, project_id, swarm_id)?;
            if let Some(member) = swarm.members.iter_mut().find(|item| item.nick == nick) {
                if member.thread == choosing_thread {
                    member.thread = previous.or_else(|| member.merged_threads.last().copied());
                }
                member.left_reason = Some(reason);
            }
        }
        if let Some(thread_id) = choosing_thread
            && let Some(started) = next.thread_started_at.get_mut(&project_id)
        {
            started.remove(&thread_id);
        }
        self.commit(next)?;
        Ok(())
    }

    /// Appends a line to a goal swarm's activity feed.
    pub fn record_goal_swarm_event(
        &mut self,
        project_id: u32,
        swarm_id: u32,
        event: GoalFeedEvent,
    ) -> Result<bool, ThreadError> {
        let mut next = self.state.clone();
        let Some(swarm) = project_mut(&mut next, project_id)?
            .goal_swarms
            .iter_mut()
            .find(|item| item.id == swarm_id)
        else {
            return Ok(false);
        };
        push_feed(swarm, event);
        self.commit(next)?;
        Ok(true)
    }

    /// Records a member's merge and, when `keep_going` is on, returns its slot so the same
    /// nick can claim the next open piece in a fresh worktree (`SPEC.md` F16 step 5).
    pub fn prepare_keep_going(
        &mut self,
        project_id: u32,
        merged_thread: u32,
    ) -> Result<Option<GoalAgentSlot>, ThreadError> {
        let mut next = self.state.clone();
        let project = project_mut(&mut next, project_id)?;
        let Some(thread) = project.threads.iter().find(|thread| {
            thread.id == merged_thread
                && matches!(thread.state, ThreadState::Merged | ThreadState::Done)
        }) else {
            return Ok(None);
        };
        let landed = thread.state == ThreadState::Merged;
        let Some(goal_member) = thread.goal_member.clone().filter(|member| !member.choosing) else {
            return Ok(None);
        };
        let Some(swarm) = project
            .goal_swarms
            .iter_mut()
            .find(|item| item.id == goal_member.swarm_id)
        else {
            return Ok(None);
        };
        let Some(member) = swarm
            .members
            .iter_mut()
            .find(|item| item.nick == goal_member.nick)
        else {
            return Ok(None);
        };
        if landed && !member.merged_threads.contains(&merged_thread) {
            member.merged_threads.push(merged_thread);
        }
        let continuing = swarm.keep_going
            && member.thread == Some(merged_thread)
            && member.left_reason.is_none();
        let next_slot = continuing.then(|| GoalAgentSlot {
            swarm_id: swarm.id,
            prompt: swarm.prompt.clone(),
            sources: swarm.sources,
            nick: member.nick.clone(),
            provider: member.provider,
            model_name: member.model_name.clone(),
            effort: member.effort,
            speed: member.speed,
            keep_going: true,
            previous_work_thread: Some(merged_thread),
        });
        self.commit(next)?;
        Ok(next_slot)
    }
}

fn require_goal_gate(state: &crate::PersistedState, project_id: u32) -> Result<(), ThreadError> {
    require_launch(state, LaunchRequest::GoalSwarm)
        .map_err(|error| ThreadError::InvalidLaunch(error.to_string()))?;
    let active = crate::project_layout::active_project(state).map(|project| project.id);
    if active != Some(project_id) {
        return Err(ThreadError::InvalidLaunch(
            "The selected project changed".into(),
        ));
    }
    Ok(())
}

fn validated_rows(
    mix: &[GoalMixRow],
    settings: &crate::Settings,
) -> Result<Vec<GoalMixRow>, ThreadError> {
    let rows: Vec<GoalMixRow> = mix.iter().copied().filter(|row| row.count > 0).collect();
    let total: u32 = rows
        .iter()
        .fold(0, |sum, row| sum.saturating_add(row.count));
    if rows.is_empty() || total == 0 || total > MAXIMUM_AGENTS {
        return Err(ThreadError::InvalidLaunch(format!(
            "Choose between 1 and {MAXIMUM_AGENTS} agents"
        )));
    }
    if let Some(row) = rows
        .iter()
        .find(|row| !settings.provider_usable(row.provider))
    {
        return Err(ThreadError::InvalidLaunch(format!(
            "{} isn't signed in",
            settings.provider_name(row.provider)
        )));
    }
    Ok(rows)
}

fn normalized_prompt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        DEFAULT_PROMPT.into()
    } else {
        trimmed.into()
    }
}

fn build_slots(
    settings: &Settings,
    project: &ProjectItem,
    swarm_id: u32,
    prompt: &str,
    sources: WorkSources,
    keep_going: bool,
    rows: &[GoalMixRow],
) -> (Vec<GoalSwarmMember>, Vec<GoalAgentSlot>) {
    let stem_counts: Vec<(String, u32)> = rows
        .iter()
        .map(|row| {
            let model_name = provider_model_name(settings, row.provider);
            (goal_swarm::model_nick(&model_name), row.count)
        })
        .collect();
    let nicks = goal_swarm::allocate_nicks(&stem_counts, &used_nick_numbers(project));
    let mut nick_iter = nicks.into_iter();
    let mut members = Vec::new();
    let mut slots = Vec::new();
    for row in rows {
        let model_name = provider_model_name(settings, row.provider);
        for _ in 0..row.count {
            let nick = nick_iter.next().unwrap_or_else(|| "agent-1".into());
            members.push(GoalSwarmMember {
                nick: nick.clone(),
                provider: row.provider,
                model_name: model_name.clone(),
                effort: row.effort,
                speed: row.speed,
                thread: None,
                left_reason: None,
                merged_threads: Vec::new(),
            });
            slots.push(GoalAgentSlot {
                swarm_id,
                prompt: prompt.to_owned(),
                sources,
                nick,
                provider: row.provider,
                model_name: model_name.clone(),
                effort: row.effort,
                speed: row.speed,
                keep_going,
                previous_work_thread: None,
            });
        }
    }
    (members, slots)
}

fn bind_piece_owner(
    project: &mut ProjectItem,
    thread_id: u32,
    piece: &WorkPiece,
) -> Result<(), ThreadError> {
    match &piece.key {
        WorkKey::Task { identifier } => {
            let completed = project
                .rosen_work_item(&piece.key)
                .is_some_and(|item| item.status.completed());
            let task = project
                .tasks
                .iter_mut()
                .find(|entry| entry.id == *identifier)
                .ok_or(ThreadError::UnknownTask(*identifier))?;
            if completed || task.state != TaskState::Todo || task.assigned_thread_id.is_some() {
                return Err(ThreadError::TaskUnavailable(*identifier));
            }
            task.state = TaskState::Now;
            task.assigned_thread_id = Some(thread_id);
            if let Some(record) = project.threads.iter_mut().find(|item| item.id == thread_id) {
                record.task_ids.push(*identifier);
            }
        }
        WorkKey::Issue { repository, number } => {
            let issue = ThreadIssue::new(repository.clone(), *number);
            if project.rosen_issue_completed(&issue) {
                return Err(ThreadError::IssueCompleted(*number));
            }
            if let Some(existing) = issue_thread(project, &issue)
                && existing.id != thread_id
            {
                return Err(ThreadError::IssueAssigned {
                    number: *number,
                    thread: existing.id,
                });
            }
            if let Some(record) = project.threads.iter_mut().find(|item| item.id == thread_id) {
                record.issue = Some(issue);
            }
        }
        WorkKey::Spec { section } => {
            let taken = project.built_spec_sections.contains(section)
                || project.threads.iter().any(|item| {
                    !item.supervision.released
                        && item.id != thread_id
                        && item.spec_section.as_ref() == Some(section)
                });
            if taken {
                return Err(ThreadError::SpecUnavailable(section.clone()));
            }
            if let Some(record) = project.threads.iter_mut().find(|item| item.id == thread_id) {
                record.spec_section = Some(section.clone());
            }
        }
    }
    Ok(())
}

fn remove_untouched_choosing_thread(project: &mut ProjectItem, thread_id: u32) {
    let removable = project.threads.iter().position(|thread| {
        thread.id == thread_id
            && thread.pull_request_number.is_none()
            && thread.task_ids.is_empty()
            && thread.issue.is_none()
            && thread.spec_section.is_none()
    });
    if let Some(position) = removable {
        let removed = project.threads.remove(position);
        if project.active_thread_id == Some(thread_id) {
            project.active_thread_id = removed
                .previous_work_thread
                .or_else(|| project.threads.first().map(|thread| thread.id));
        }
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

fn goal_swarm_mut(
    project: &mut ProjectItem,
    project_id: u32,
    swarm_id: u32,
) -> Result<&mut GoalSwarmRecord, ThreadError> {
    project
        .goal_swarms
        .iter_mut()
        .find(|item| item.id == swarm_id)
        .ok_or(ThreadError::UnknownThread {
            project: project_id,
            thread: swarm_id,
        })
}

fn push_feed(swarm: &mut GoalSwarmRecord, event: GoalFeedEvent) {
    swarm.feed.push(event);
    let overflow = swarm.feed.len().saturating_sub(FEED_LIMIT);
    swarm.feed.drain(..overflow);
}
