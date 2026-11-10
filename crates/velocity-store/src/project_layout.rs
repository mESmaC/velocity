//! Read the active project's persisted context into the shell's public read model.

use crate::duty::DutyRoleItem;
use crate::worktree_settings::project_owners;
use crate::{
    AgentActivity, EffortLevel, InterviewRecord, InterviewStage, PersistedState, ProjectItem,
    ProviderKind, ReviewStage, SpeedMode, TaskState, ThreadItem, ThreadState,
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
    LayoutData, LayoutIntent, PaletteItem, PaletteTarget, ProviderColor, RailItem, ThreadEntry,
};
use velocity_core::navigation::NavigationView;
use velocity_core::thread_view::ThreadLifecycle;
use velocity_core::thread_view_model::ThreadDetailEntry;
use velocity_core::worktree_view::footer_summary;

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
        data.project_identifier = Some(project.id);
        data.project_name = project.name.clone();
        let merged_today = project.merged_today as usize;
        let task_count = project.tasks.len();
        let mut entries: Vec<ThreadEntry> = project
            .duty_roles
            .iter()
            .map(|duty| duty_entry(duty, merged_today))
            .collect();
        data.swarms = crate::swarms::swarm_entries(project);
        entries.extend(data.swarms.iter().map(crate::swarms::swarm_row));
        data.goal_swarms = crate::goal_swarm_view::goal_swarm_entries(project);
        entries.extend(
            data.goal_swarms
                .iter()
                .map(crate::goal_swarm_view::goal_swarm_row),
        );
        entries.extend(thread_entries(project, task_count));
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
        (data.worktree_count, data.worktree_drive) =
            footer_summary(&project_owners(project), &fallback_root(project, state));
    }
    data
}

/// Thread rows. Swarm members are listed under their swarm in agent order (a1, a2, …),
/// and goal swarm members under theirs in launch order (F16), not in the other groups
/// (F10); other threads keep newest first.
fn thread_entries(project: &ProjectItem, task_count: usize) -> Vec<ThreadEntry> {
    let mut threads: Vec<_> = project
        .threads
        .iter()
        .map(|thread| {
            let membership = crate::swarms::thread_swarm(project, thread.id)
                .map(|(swarm, agent)| (swarm.id, agent, 0))
                .or_else(|| crate::goal_swarm_view::goal_membership(project, thread));
            let mut entry = thread_entry(thread, task_count);
            entry.policy.member_of = membership.map(|(swarm, _, _)| swarm as usize);
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

fn duty_entry(item: &DutyRoleItem, merged_today: usize) -> ThreadEntry {
    let core_entry = item.to_core();
    let metadata = duty::duty_sidebar_summary(&core_entry, merged_today);
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
        title: core_entry.role.title().into(),
        metadata,
        view: NavigationView::Duty,
        detail: None,
        duty: Some(core_entry),
    }
}

fn thread_entry(thread: &ThreadItem, task_count: usize) -> ThreadEntry {
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
            Some(interview) => interview_view(interview, task_count).status_label,
            None => thread_metadata(thread),
        },
        view: match &thread.interview {
            Some(interview) => interview_navigation_view(interview_phase(interview.stage)),
            None if thread.state == ThreadState::Spec => NavigationView::SpecScan,
            None => NavigationView::Thread,
        },
        detail: Some(thread_detail(thread)),
        duty: None,
    }
}

/// The thread header and model pill from what `state.json` keeps. The
/// transcript is not persisted, so a reopened thread starts with no blocks
/// until its agent session streams new ones.
fn thread_detail(thread: &ThreadItem) -> ThreadDetailEntry {
    ThreadDetailEntry {
        identifier: thread.id as usize,
        title: thread.title.clone(),
        titling: false,
        title_model_name: String::new(),
        branch: thread.branch_name.clone(),
        worktree_path: thread.worktree_path.clone().unwrap_or_default(),
        lifecycle: thread_lifecycle(thread.state),
        added: thread.additions as usize,
        deleted: thread.deletions as usize,
        elapsed_label: elapsed_label(thread.elapsed_seconds),
        pull_request_number: thread.pull_request_number.map(|number| number as usize),
        agent_busy: matches!(
            thread.state,
            ThreadState::Working | ThreadState::Review | ThreadState::Closed
        ) && matches!(
            thread.activity,
            AgentActivity::Thinking | AgentActivity::Executing
        ),
        show_work: false,
        worked_summary: None,
        pull_request_history: thread
            .follow_through
            .as_ref()
            .filter(|watch| watch.number != 0)
            .map(|watch| watch.view()),
        blocks: Vec::new(),
        agent_commands: Vec::new(),
        provider: provider_color(thread.provider),
        model_name: thread.model_name.clone(),
        effort_label: effort_label(thread.effort).into(),
        requested_fast: thread.preferred_speed == SpeedMode::Fast,
        model_has_fast_mode: thread.model_supports_fast,
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
        ThreadState::Closed => ThreadGroup::Closed,
        ThreadState::Done => ThreadGroup::Done,
        ThreadState::Merged => ThreadGroup::Merged,
    }
}

pub(crate) fn thread_metadata(thread: &ThreadItem) -> String {
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

/// The footer names this drive while the project has no live worktree.
fn fallback_root(project: &ProjectItem, state: &PersistedState) -> String {
    let settings = &state.settings.worktrees;
    settings
        .project_roots
        .get(&project.id)
        .or(settings.location_root.as_ref())
        .cloned()
        .unwrap_or_default()
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
