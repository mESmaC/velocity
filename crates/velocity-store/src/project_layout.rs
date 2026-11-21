//! Read the active project's persisted context into the shell's public read model.

use crate::duty::DutyRoleItem;
use crate::{
    AgentActivity, EffortLevel, InterviewRecord, InterviewStage, PersistedState, ProjectItem,
    ProviderKind, ReviewStage, SpeedMode, TaskState, ThreadItem, ThreadState, VersionControl,
};
use std::collections::BTreeSet;
use velocity_core::duty;
use velocity_core::interview_view::{
    InterviewDecisionView, InterviewEntry, InterviewOptionView, InterviewPhase,
    InterviewQuestionView, InterviewView, ProofListEntryView, interview_navigation_view,
    interview_status_label,
};
use velocity_core::layout::{LayoutRow, ThreadGroup};
use velocity_core::layout_view::{
    LayoutData, LayoutIntent, PaletteItem, PaletteTarget, ProviderColor, RailItem, RosenWorkView,
    ThreadEntry,
};
use velocity_core::navigation::NavigationView;
use velocity_core::thread_view::ThreadLifecycle;
use velocity_core::thread_view_model::{
    RosenChangeStage, RosenChangeView, RosenResolutionView, ThreadDetailEntry,
};

pub(crate) fn active_project(state: &PersistedState) -> Option<&ProjectItem> {
    usize::try_from(state.active_project_index)
        .ok()
        .and_then(|index| state.projects.get(index))
}

pub(crate) fn read_model(store: &crate::StateStore) -> LayoutData {
    let state = store.state();
    let mut data = LayoutData {
        projects: project_palette(state),
        ..LayoutData::default()
    };
    if let Some(project) = active_project(state) {
        data.project_identifier = Some(project.id);
        data.project_name = project.name.clone();
        data.rosen_mirror = project.rosen_mirror.clone();
        data.collapse_repository = project.collapse_repository.clone();
        data.collapse_live_state = project.collapse_live_state;
        data.collapse_mirror_health = project
            .collapse_repository
            .as_ref()
            .and(project.collapse_mirror_health.clone());
        data.rosen_mirror_divergence = project.rosen_mirror_divergence.clone();
        // A project that left Rosen or lost its GitHub repository has no base CI to show.
        data.base_ci = project
            .github_remote
            .as_ref()
            .filter(|_| project.version_control == VersionControl::Rosen)
            .and(project.base_ci.clone());
        if project.version_control == VersionControl::Rosen {
            data.rosen_work = Some(RosenWorkView {
                items: project.rosen_work_items.clone(),
                expanded_issues: project.expanded_work_issues.clone(),
            });
        }
        data.rosen_migration_notice = project.rosen_migration_notice.clone();
        data.settled_expanded = project.settled_expanded;
        let merged_today = project.landed_on(crate::landed_today::local_day()) as usize;
        let task_count = project.tasks.len();
        let mut entries: Vec<ThreadEntry> = project.duty_roles.iter().map(duty_entry).collect();
        data.swarms = crate::swarms::swarm_entries(project);
        entries.extend(data.swarms.iter().map(crate::swarms::swarm_row));
        data.goal_swarms = crate::goal_swarm_view::goal_swarm_entries(project);
        entries.extend(
            data.goal_swarms
                .iter()
                .map(crate::goal_swarm_view::goal_swarm_row),
        );
        entries.extend(thread_entries(project, task_count, &state.settings));
        data.threads = entries;
        data.interviews = project
            .threads
            .iter()
            .filter_map(|thread| {
                Some(InterviewEntry {
                    thread_identifier: thread.id as usize,
                    view: interview_view(thread.interview.as_ref()?, task_count),
                })
            })
            .collect();
        data.tasks = crate::task_list::task_views(project);
        data.completed_task_count = project
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Done)
            .count();
        data.pull_requests = pull_requests(project);
        data.merged_today = merged_today;
    }
    data.providers = crate::provider_views::provider_usage(&state.settings);
    data.default_model_pill = Some(crate::provider_views::default_model_pill(&state.settings));
    data.composer_model_pill = store.composer_model_pill();
    data
}

/// Thread rows. Swarm members are listed under their swarm in agent order (a1, a2, …),
/// and goal swarm members under theirs in launch order (F16), not in the other groups
/// (F10); other threads keep newest first.
fn thread_entries(
    project: &ProjectItem,
    task_count: usize,
    settings: &crate::Settings,
) -> Vec<ThreadEntry> {
    let mut threads: Vec<_> = project
        .threads
        .iter()
        .map(|thread| {
            let membership = crate::swarms::thread_swarm(project, thread.id)
                .map(|(swarm, agent)| (swarm.id, agent, 0))
                .or_else(|| crate::goal_swarm_view::goal_membership(project, thread));
            let mut entry = thread_entry(thread, task_count, settings);
            entry.policy.member_of = membership
                .filter(|_| entry.policy.group != ThreadGroup::Settled)
                .map(|(swarm, _, _)| swarm as usize);
            (membership, entry)
        })
        .collect();
    threads.sort_by_key(|(membership, _)| *membership);
    threads.into_iter().map(|(_, entry)| entry).collect()
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

fn duty_entry(item: &DutyRoleItem) -> ThreadEntry {
    let core_entry = item.to_core();
    let metadata = duty::duty_sidebar_summary(&core_entry);
    ThreadEntry {
        policy: LayoutRow {
            identifier: core_entry.identifier,
            group: ThreadGroup::Duty,
            hidden: false,
            matches_search: true,
            member_of: None,
            expanded: false,
            agent_count: core_entry.agents.len(),
            paused: core_entry.paused,
        },
        title: core_entry.title().into(),
        metadata,
        view: NavigationView::Duty,
        detail: None,
        duty: Some(core_entry),
    }
}

fn thread_entry(thread: &ThreadItem, task_count: usize, settings: &crate::Settings) -> ThreadEntry {
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
        metadata: match &thread.interview {
            Some(_) if thread.provider_queued => thread_metadata(thread),
            Some(interview) => interview_view(interview, task_count).status_label,
            None => thread_metadata(thread),
        },
        view: match &thread.interview {
            Some(interview) => interview_navigation_view(interview_phase(interview.stage)),
            None if thread.state == ThreadState::Spec => NavigationView::SpecScan,
            None => NavigationView::Thread,
        },
        detail: Some(thread_detail(thread, settings)),
        duty: None,
    }
}

/// The thread header, conversation and model pill retained in `state.json`.
fn thread_detail(thread: &ThreadItem, settings: &crate::Settings) -> ThreadDetailEntry {
    let rosen_change = rosen_change(thread);
    ThreadDetailEntry {
        identifier: thread.id as usize,
        title: thread.title.clone(),
        titling: false,
        title_model_name: String::new(),
        account_label: settings.account_label(thread.provider, thread.provider_account),
        usage_account: Some(crate::provider_views::account_key(
            thread.provider,
            thread.provider_account,
        )),
        branch: thread.branch_name.clone(),
        worktree_path: thread.worktree_path.clone().unwrap_or_default(),
        lifecycle: thread_lifecycle(thread.state),
        added: thread.additions as usize,
        deleted: thread.deletions as usize,
        elapsed_label: elapsed_label(thread.elapsed_seconds),
        pull_request_number: if rosen_change.is_none() {
            thread.pull_request_number.map(|number| number as usize)
        } else {
            None
        },
        agent_busy: matches!(
            thread.state,
            ThreadState::Working | ThreadState::Review | ThreadState::Closed
        ) && matches!(
            thread.activity,
            AgentActivity::Thinking | AgentActivity::Executing
        ),
        read_only: thread.role.is_read_only(),
        no_change_reason: thread.no_change_reason.clone(),
        show_work: false,
        worked_summary: None,
        rosen_change,
        blocks: thread.transcript.clone(),
        previous_work_thread: thread.previous_work_thread.map(|id| id as usize),
        agent_commands: Vec::new(),
        provider: provider_color(thread.provider),
        model_name: thread.model_name.clone(),
        effort_label: effort_label(thread.effort).into(),
        requested_fast: thread.preferred_speed == SpeedMode::Fast,
        model_has_fast_mode: thread.model_supports_fast,
    }
}

fn rosen_change(thread: &ThreadItem) -> Option<RosenChangeView> {
    let identifier = thread.rosen_change.as_ref()?;
    let mut view = if let Some(view) = &thread.rosen_change_view
        && &view.identifier == identifier
    {
        view.clone()
    } else {
        RosenChangeView::new(identifier.clone(), rosen_stage(thread))
    };
    let has_pending_conflict = thread
        .rosen_follow_ups
        .iter()
        .any(|pending| matches!(pending, crate::rosen::RosenFollowUp::Conflict { .. }));
    view.resolution = if !view.conflicts.is_empty()
        && (has_pending_conflict || thread.rosen_resolution_status.is_some())
    {
        let status = thread.rosen_resolution_status.as_ref();
        Some(RosenResolutionView {
            pending: has_pending_conflict,
            status: status.map_or_else(
                || crate::rosen::DEFAULT_RESOLUTION_STATUS.to_owned(),
                |entry| entry.status.clone(),
            ),
            offer_abandon: status.is_some_and(|entry| entry.offer_abandon),
        })
    } else {
        None
    };
    view.follow_up_error = thread.rosen_follow_up_error.clone();
    Some(view)
}

fn rosen_stage(thread: &ThreadItem) -> RosenChangeStage {
    if let Some(view) = &thread.rosen_change_view
        && thread.rosen_change.as_ref() == Some(&view.identifier)
    {
        return view.stage;
    }
    match (thread.state, thread.review_stage) {
        (ThreadState::Merged, _) => RosenChangeStage::Landed,
        (ThreadState::Done, _) => RosenChangeStage::Abandoned,
        (ThreadState::Review, Some(ReviewStage::Review)) => RosenChangeStage::Named,
        _ => RosenChangeStage::Editing,
    }
}

fn thread_lifecycle(state: ThreadState) -> ThreadLifecycle {
    match state {
        ThreadState::Draft => ThreadLifecycle::Draft,
        // A spec interview is live agent work in its own checkout.
        ThreadState::Spec | ThreadState::Working => ThreadLifecycle::Working,
        ThreadState::Review => ThreadLifecycle::Review,
        ThreadState::Error => ThreadLifecycle::Error,
        // A locked spec thread finished without a merge, so its checkout stays.
        ThreadState::Closed | ThreadState::Done => ThreadLifecycle::Closed,
        ThreadState::Merged => ThreadLifecycle::Merged,
    }
}

pub(crate) fn provider_color(provider: ProviderKind) -> ProviderColor {
    match provider {
        ProviderKind::Claude => ProviderColor::Claude,
        ProviderKind::Codex => ProviderColor::Codex,
        ProviderKind::Antigravity => ProviderColor::Antigravity,
        ProviderKind::Kimi => ProviderColor::Kimi,
        ProviderKind::Custom(_) => ProviderColor::Custom,
    }
}

pub(crate) fn effort_label(effort: EffortLevel) -> &'static str {
    match effort {
        EffortLevel::Low => "Low",
        EffortLevel::Medium => "Medium",
        EffortLevel::High => "High",
        EffortLevel::Max => "Max",
    }
}

/// The reference's compact largest-unit time: `0s`, `6m`, `3h`.
fn elapsed_label(seconds: u32) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

fn thread_group(state: ThreadState) -> ThreadGroup {
    match state {
        ThreadState::Draft => ThreadGroup::Draft,
        ThreadState::Spec => ThreadGroup::Spec,
        ThreadState::Working => ThreadGroup::Working,
        ThreadState::Review => ThreadGroup::Review,
        ThreadState::Error => ThreadGroup::Error,
        ThreadState::Closed => ThreadGroup::Settled,
        ThreadState::Done => ThreadGroup::Settled,
        ThreadState::Merged => ThreadGroup::Settled,
    }
}

pub(crate) fn thread_metadata(thread: &ThreadItem) -> String {
    if thread.no_change_reason.is_some() {
        return "Ended with no change".into();
    }
    if let Some(refusal) = &thread.rosen_land_refusal {
        return refusal.clone();
    }
    if thread.provider_queued
        && thread.activity != AgentActivity::Idle
        && matches!(
            thread.state,
            ThreadState::Working | ThreadState::Review | ThreadState::Spec
        )
    {
        return format!("queued \u{b7} {}", thread.provider.key());
    }
    if thread.activity == AgentActivity::Stalled {
        return "stalled".into();
    }
    if thread.activity == AgentActivity::AtLimit {
        return thread.supervision.retry_after.map_or_else(
            || "at limit".into(),
            |reset| {
                let hour = (reset % 86_400) / 3_600;
                let minute = (reset % 3_600) / 60;
                format!("at limit - retry {hour:02}:{minute:02} UTC")
            },
        );
    }
    if let Some(waiting) = &thread.dependency_wait {
        return waiting.clone();
    }
    if thread.state == ThreadState::Working
        && thread
            .goal_member
            .as_ref()
            .is_some_and(|member| member.choosing)
    {
        return "choosing work".into();
    }
    if thread.rosen_change.is_some() {
        return rosen_stage(thread).label().into();
    }
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

fn interview_phase(stage: InterviewStage) -> InterviewPhase {
    match stage {
        InterviewStage::Scan => InterviewPhase::Scan,
        InterviewStage::Ask => InterviewPhase::Ask,
        InterviewStage::Ready => InterviewPhase::Ready,
        InterviewStage::Locked => InterviewPhase::Locked,
    }
}

fn interview_view(interview: &InterviewRecord, task_count: usize) -> InterviewView {
    let phase = interview_phase(interview.stage);
    // The reference numbers questions by answers so far: the next one is decided + 1.
    let asked = interview.decisions.len() + 1;
    let question = interview
        .question
        .as_ref()
        .filter(|_| phase == InterviewPhase::Ask)
        .map(|question| InterviewQuestionView {
            number: asked,
            key: question.key.clone(),
            text: question.text.clone(),
            why: question.why.clone(),
            options: question
                .options
                .iter()
                .map(|option| InterviewOptionView {
                    label: option.label.clone(),
                    detail: option.detail.clone(),
                    recommended: option.recommended,
                })
                .collect(),
        });
    let decided = interview.decisions.len();
    InterviewView {
        phase,
        brief: interview.brief.clone(),
        summary: interview.summary.clone(),
        question,
        decisions: interview
            .decisions
            .iter()
            .map(|decision| InterviewDecisionView {
                key: decision.key.clone(),
                value: decision.value.clone(),
            })
            .collect(),
        proof_list: interview
            .proof_list
            .iter()
            .map(|entry| ProofListEntryView {
                feature: entry.feature.clone(),
                included: entry.included,
            })
            .collect(),
        task_count,
        status_label: interview_status_label(phase, asked, decided, task_count),
        notice: interview.notice.clone(),
    }
}
