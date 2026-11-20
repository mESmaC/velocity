//! Saved per-project continuous dispatch (F8).
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeMap;

use crate::issues::ThreadIssue;
use crate::{
    ProjectItem, StateStore, TaskItem, TaskState, ThreadState, VersionControl, threads::ThreadError,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ContinuousDispatch {
    #[serde(default)]
    pub slots: u32,
    #[serde(default)]
    pub paused: bool,
    /// Do not send an unchanged open issue to another agent after a no-change result or
    /// while GitHub still shows an issue whose change just landed.
    #[serde(default)]
    pub attempts: Vec<DispatchAttempt>,
    /// A task whose turn ended with no change waits for an edit or an explicit reopen.
    #[serde(default)]
    #[specta(skip)]
    pub(crate) task_attempts: BTreeMap<u32, String>,
    /// Empty threads whose stopped workspace and claims still need cleanup.
    #[serde(default)]
    pub cleanup: Vec<u32>,
    /// Issue reports must survive workspace cleanup, thread deletion and failed GitHub calls.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[specta(skip)]
    pub no_change_reports: BTreeMap<u32, NoChangeReport>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[specta(skip)]
    pub issue_overlaps: BTreeMap<u32, IssueOverlap>,
    /// Open issues whose one overlap hold has ended (#525). They are never held on
    /// overlap again, so a hold cannot repeat behind each next running issue.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[specta(skip)]
    pub overlap_released: BTreeMap<u32, ThreadIssue>,
    /// Issues whose GitHub record shows a Velocity land still waiting for a green
    /// integration build (#504). They survive thread deletion, so the issue is not sent
    /// to another agent that can only find nothing to do.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[specta(skip)]
    pub landed_holds: BTreeMap<u32, LandedHold>,
    /// Dispatch threads that failed before their agent's first turn (#514), by thread.
    /// The issue is already back in dispatch; the reason still has to reach it as a
    /// comment, across restarts and failed GitHub calls.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[specta(skip)]
    pub launch_failure_reports: BTreeMap<u32, LaunchFailureReport>,
    /// How often in a row each issue's thread failed before starting (#514), by issue
    /// number. A launch that keeps failing must not start threads for ever.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[specta(skip)]
    pub unstarted_failures: BTreeMap<u32, UnstartedFailures>,
}

/// How many times in a row an issue's thread may fail before its first turn and be
/// dispatched again on the next pass (#514). Later failures still return the issue, but
/// dispatch then waits before each further try, so a launch that cannot succeed neither
/// starts threads for ever nor fills the issue with comments.
pub const UNSTARTED_RETURNS: u32 = 3;

/// The wait before the first try past [`UNSTARTED_RETURNS`]. It doubles with each further
/// failure, up to a day.
const UNSTARTED_WAIT_SECONDS: u64 = 60 * 60;

/// Why a dispatch thread never started, waiting to be posted on its issue (#514).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchFailureReport {
    pub issue: ThreadIssue,
    #[serde(default)]
    pub reason: String,
    /// Which failure in a row this is for the issue, from 1.
    #[serde(default)]
    pub failure: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnstartedFailures {
    pub issue: ThreadIssue,
    #[serde(default)]
    pub count: u32,
    /// Unix seconds of the newest failure.
    #[serde(default)]
    pub last: u64,
}

impl UnstartedFailures {
    /// Unix seconds until which dispatch leaves the issue alone; `None` while it may be
    /// tried again at once.
    pub fn held_until(&self) -> Option<u64> {
        let past = self.count.checked_sub(UNSTARTED_RETURNS + 1)?;
        let wait = UNSTARTED_WAIT_SECONDS
            .saturating_mul(1u64.checked_shl(past).unwrap_or(u64::MAX))
            .min(24 * 60 * 60);
        Some(self.last.saturating_add(wait))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedHold {
    pub issue: ThreadIssue,
    #[serde(default)]
    pub commit: String,
    /// The issue version the comments were read at. A newer version is read again.
    #[serde(default)]
    pub updated_at: String,
    /// Unix seconds when Velocity first saw the land waiting.
    #[serde(default)]
    pub since: u64,
}

/// A landed change waiting for the integration build that closes its issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandedWait {
    pub commit: Option<String>,
    /// Unix seconds of the land, when known.
    pub since: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueOverlap {
    pub issue: ThreadIssue,
    pub blocker: u32,
    #[serde(default)]
    pub blocker_thread: Option<u32>,
    #[serde(default)]
    pub review_commit: Option<String>,
    /// Files the blocker's open change has edited that this issue also names (#525).
    #[serde(default)]
    pub paths: Vec<String>,
}

impl IssueOverlap {
    pub fn new(
        issue: ThreadIssue,
        blocker: u32,
        blocker_thread: Option<u32>,
        paths: Vec<String>,
    ) -> Self {
        Self {
            issue,
            blocker,
            blocker_thread,
            review_commit: None,
            paths,
        }
    }

    /// The short state shown on the issue's Work card.
    pub fn label(&self) -> String {
        if self.review_commit.is_some() {
            format!("Checking against #{}", self.blocker)
        } else {
            format!("Held behind #{}", self.blocker)
        }
    }

    /// Why continuous dispatch has not started the issue, for its Work card.
    pub fn reason(&self) -> String {
        if self.review_commit.is_some() {
            format!(
                "#{} landed. A review checks this issue against that land before dispatch.",
                self.blocker
            )
        } else {
            format!(
                "#{}'s open change edits files this issue names. \
                 Dispatch starts this issue when that change lands or ends.",
                self.blocker
            )
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoChangeReport {
    pub issue: crate::issues::ThreadIssue,
    pub change: String,
    pub nick: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DispatchAttempt {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    pub updated_at: String,
}

impl ContinuousDispatch {
    /// Issues whose launches failed too often in a row to be tried again yet (#514).
    pub fn launch_holds(&self, now: u64) -> impl Iterator<Item = &ThreadIssue> {
        self.unstarted_failures
            .values()
            .filter(move |failures| failures.held_until().is_some_and(|until| now < until))
            .map(|failures| &failures.issue)
    }

    /// Counts one more failed start for `issue` and returns how many it has had in a row.
    fn count_unstarted_failure(&mut self, issue: &ThreadIssue, now: u64) -> u32 {
        let count = self
            .unstarted_failures
            .get(&issue.number)
            .filter(|counted| same_issue(&counted.issue, issue))
            .map_or(0, |counted| counted.count)
            .saturating_add(1);
        self.unstarted_failures.insert(
            issue.number,
            UnstartedFailures {
                issue: issue.clone(),
                count,
                last: now,
            },
        );
        count
    }

    pub fn reporting_no_change(&self, repository: &str, number: u32) -> bool {
        self.no_change_reports.values().any(|report| {
            report.issue.number == number
                && report.issue.repository.eq_ignore_ascii_case(repository)
        })
    }

    pub fn attempted(&self, repository: &str, number: u32, updated_at: &str) -> bool {
        self.attempts.iter().any(|attempt| {
            attempt.repository.eq_ignore_ascii_case(repository)
                && attempt.number == number
                && attempt.updated_at == updated_at
        })
    }
}

impl ProjectItem {
    /// The overlap hold that keeps `issue` out of continuous dispatch, if any (#525).
    pub fn overlap_hold(&self, issue: &ThreadIssue) -> Option<&IssueOverlap> {
        self.continuous_dispatch
            .issue_overlaps
            .get(&issue.number)
            .filter(|wait| {
                wait.issue
                    .repository
                    .eq_ignore_ascii_case(&issue.repository)
            })
    }

    /// Whether `issue` has a landed change waiting for a green integration build (#504).
    /// The work projection is authoritative when it tracks the issue; otherwise a merged
    /// thread or the GitHub hold saved by dispatch is the evidence. `updated_at` limits the
    /// hold to the issue version its comments were read at; `None` accepts any version.
    pub fn landed_awaiting_integration(
        &self,
        issue: &ThreadIssue,
        updated_at: Option<&str>,
    ) -> Option<LandedWait> {
        let key = velocity_core::work_pool::WorkKey::issue(&issue.repository, issue.number);
        if let Some(item) = self.rosen_work_item(&key) {
            return (item.status == velocity_core::work_pool::WorkStatus::Landed).then(|| {
                LandedWait {
                    commit: item.landed_commit.clone(),
                    since: item.landed_at.as_deref().and_then(|at| at.parse().ok()),
                }
            });
        }
        let merged = self.threads.iter().find_map(|thread| {
            let assigned = thread.issue.as_ref()?;
            (thread.state == ThreadState::Merged
                && assigned.number == issue.number
                && assigned.repository.eq_ignore_ascii_case(&issue.repository))
            .then(|| thread.rosen_issue.landed_commit())
            .flatten()
        });
        if let Some(commit) = merged {
            return Some(LandedWait {
                commit: Some(commit.to_owned()),
                since: None,
            });
        }
        self.continuous_dispatch
            .landed_holds
            .get(&issue.number)
            .filter(|hold| {
                hold.issue
                    .repository
                    .eq_ignore_ascii_case(&issue.repository)
                    && updated_at.is_none_or(|updated_at| hold.updated_at == updated_at)
            })
            .map(|hold| LandedWait {
                commit: Some(hold.commit.clone()),
                since: Some(hold.since),
            })
    }
}

pub fn busy_count(project: &ProjectItem) -> u32 {
    project
        .threads
        .iter()
        .filter(|thread| {
            thread.interview.is_none()
                && !thread.supervision.released
                && (matches!(thread.state, ThreadState::Working | ThreadState::Review)
                    || (thread.started_by_dispatch
                        && (project.continuous_dispatch.cleanup.contains(&thread.id)
                            || thread.rosen_follow_ups.iter().any(|follow_up| {
                                matches!(
                                    follow_up,
                                    crate::rosen::RosenFollowUp::FinishLanded
                                        | crate::rosen::RosenFollowUp::FinishAbandoned
                                )
                            }))))
        })
        .count() as u32
}

pub fn has_capacity(project: &ProjectItem) -> bool {
    project.version_control == VersionControl::Rosen
        && project.spec_locked
        && !project.continuous_dispatch.paused
        && busy_count(project) < project.continuous_dispatch.slots
}

pub fn available_task(project: &ProjectItem, task: &TaskItem) -> bool {
    task.state == TaskState::Todo
        && task.assigned_thread_id.is_none()
        && !project
            .rosen_work_item(&velocity_core::work_pool::WorkKey::Task {
                identifier: task.id,
            })
            .is_some_and(|item| item.status.completed())
        && project.continuous_dispatch.task_attempts.get(&task.id) != Some(&task.text)
}

/// Reset activity is not evidence that saved user work was read. Both the admission
/// check and the final saved transition must reject unread messages.
pub fn can_finish_empty_dispatch(thread: &crate::ThreadItem) -> bool {
    thread.interview.is_none()
        && thread.state == ThreadState::Working
        && thread.activity == crate::AgentActivity::Idle
        && !thread.startup_recovery_pending
        && !crate::handoff::note_for(thread).is_some_and(|note| note.pending)
        && !thread.transcript.iter().any(|block| {
            matches!(
                block,
                velocity_core::thread_view_model::TranscriptBlock::UserMessage { queued: true, .. }
            )
        })
}

fn same_issue(left: &ThreadIssue, right: &ThreadIssue) -> bool {
    left.number == right.number && left.repository.eq_ignore_ascii_case(&right.repository)
}

/// A dispatch thread whose launch failed before its agent's session began: it did no
/// work, so the failure says nothing about its issue (#514). A session that began may
/// have left edits in the workspace, so such a thread keeps its issue.
pub fn failed_before_first_turn(thread: &crate::ThreadItem) -> bool {
    thread.started_by_dispatch
        && thread.state == ThreadState::Error
        && thread.supervision.turns == 0
        && thread.agent_session.is_none()
        && !thread.supervision.released
        && thread.interview.is_none()
}

impl StateStore {
    /// Saves the current overlap holds with the issues whose hold has ended (#525). Both
    /// go in one write, so a restart never sees an ended hold without its release.
    pub fn save_issue_overlaps(
        &mut self,
        project: u32,
        overlaps: BTreeMap<u32, IssueOverlap>,
        released: BTreeMap<u32, ThreadIssue>,
    ) -> Result<(), ThreadError> {
        let item = self
            .project(project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if item.continuous_dispatch.issue_overlaps == overlaps
            && item.continuous_dispatch.overlap_released == released
        {
            return Ok(());
        }
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        item.continuous_dispatch.issue_overlaps = overlaps;
        item.continuous_dispatch.overlap_released = released;
        self.commit(next)?;
        Ok(())
    }

    /// Records GitHub evidence that `issue` landed and waits for integration (#504). A
    /// re-read of the same land keeps the first time it was seen, so the stuck-integration
    /// notice measures the whole wait.
    pub fn save_landed_hold(
        &mut self,
        project: u32,
        issue: ThreadIssue,
        commit: String,
        updated_at: String,
        now: u64,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let since = item
            .continuous_dispatch
            .landed_holds
            .get(&issue.number)
            .filter(|hold| {
                hold.commit == commit
                    && hold
                        .issue
                        .repository
                        .eq_ignore_ascii_case(&issue.repository)
            })
            .map_or(now, |hold| hold.since);
        let hold = LandedHold {
            issue,
            commit,
            updated_at,
            since,
        };
        let holds = &mut item.continuous_dispatch.landed_holds;
        if holds.get(&hold.issue.number) == Some(&hold) {
            return Ok(());
        }
        holds.insert(hold.issue.number, hold);
        self.commit(next)?;
        Ok(())
    }

    /// Drops holds for issues that left the open backlog or were found no longer landed.
    pub fn prune_landed_holds(
        &mut self,
        project: u32,
        repository: &str,
        keep: impl Fn(u32) -> bool,
    ) -> Result<(), ThreadError> {
        let item = self
            .project(project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let retained = |number: &u32, hold: &LandedHold| {
            hold.issue.repository.eq_ignore_ascii_case(repository) && keep(*number)
        };
        if item
            .continuous_dispatch
            .landed_holds
            .iter()
            .all(|(number, hold)| retained(number, hold))
        {
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            item.continuous_dispatch
                .landed_holds
                .retain(|number, hold| retained(number, hold));
        }
        self.commit(next)?;
        Ok(())
    }

    /// One durable reservation for the whole startup batch, before activity is reset.
    pub fn reserve_startup_recovery(
        &mut self,
        interrupted: &[(u32, u32)],
    ) -> Result<(), crate::StoreError> {
        let mut next = self.state.clone();
        for (project, identifier) in interrupted {
            if let Some(thread) = next
                .projects
                .iter_mut()
                .find(|item| item.id == *project)
                .and_then(|project| {
                    project
                        .threads
                        .iter_mut()
                        .find(|thread| thread.id == *identifier)
                })
            {
                thread.startup_recovery_pending = true;
            }
        }
        if next != self.state {
            self.commit(next)?;
        }
        Ok(())
    }

    pub fn set_continuous_dispatch(
        &mut self,
        project: u32,
        slots: u32,
        paused: bool,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if item.version_control != VersionControl::Rosen {
            return Err(ThreadError::InvalidLaunch(
                "Continuous dispatch needs a Rosen project".into(),
            ));
        }
        if item.continuous_dispatch.slots == slots && item.continuous_dispatch.paused == paused {
            return Ok(());
        }
        item.continuous_dispatch.slots = slots;
        item.continuous_dispatch.paused = paused;
        self.commit(next)?;
        Ok(())
    }

    /// Save ownership and the attempted issue version together, without stealing focus.
    pub fn record_continuous_dispatch(
        &mut self,
        project: u32,
        thread: crate::threads::NewThread,
        issue: crate::issues::ThreadIssue,
        updated_at: String,
    ) -> Result<u32, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if !has_capacity(item)
            || item
                .continuous_dispatch
                .attempted(&issue.repository, issue.number, &updated_at)
            || item
                .continuous_dispatch
                .reporting_no_change(&issue.repository, issue.number)
            || item
                .continuous_dispatch
                .issue_overlaps
                .get(&issue.number)
                .is_some_and(|wait| {
                    wait.issue
                        .repository
                        .eq_ignore_ascii_case(&issue.repository)
                })
        {
            return Err(ThreadError::InvalidLaunch(
                "Dispatch settings or capacity changed".into(),
            ));
        }
        let selected = item.active_thread_id;
        let attempt = DispatchAttempt {
            repository: issue.repository.clone(),
            number: issue.number,
            updated_at,
        };
        let identifier = crate::threads::insert_thread(&mut next, project, thread, Some(issue))?;
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            mark_dispatch(item, selected, identifier);
            item.continuous_dispatch.attempts.retain(|prior| {
                prior.number != attempt.number
                    || !prior.repository.eq_ignore_ascii_case(&attempt.repository)
            });
            item.continuous_dispatch.attempts.push(attempt);
        }
        self.commit(next)?;
        Ok(identifier)
    }

    /// Save the thread and its task ownership together before starting an ACP session.
    pub fn record_continuous_task(
        &mut self,
        project: u32,
        thread: crate::threads::NewThread,
        task: u32,
    ) -> Result<u32, ThreadError> {
        let item = self
            .project(project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if !has_capacity(item) {
            return Err(ThreadError::InvalidLaunch(
                "Dispatch settings or capacity changed".into(),
            ));
        }
        if !item
            .tasks
            .iter()
            .any(|entry| entry.id == task && available_task(item, entry))
        {
            return Err(ThreadError::TaskUnavailable(task));
        }
        let selected = item.active_thread_id;
        let mut next = self.state.clone();
        let identifier = crate::threads::insert_thread(&mut next, project, thread, None)?;
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        crate::swarms::claim_task(item, task, identifier)?;
        mark_dispatch(item, selected, identifier);
        self.commit(next)?;
        Ok(identifier)
    }
}

fn mark_dispatch(project: &mut ProjectItem, selected: Option<u32>, thread: u32) {
    let previous = project
        .threads
        .iter()
        .find(|entry| {
            entry.started_by_dispatch
                && matches!(entry.state, ThreadState::Done | ThreadState::Merged)
                && entry.rosen_follow_ups.is_empty()
                && !project.continuous_dispatch.cleanup.contains(&entry.id)
                && !project
                    .threads
                    .iter()
                    .any(|later| later.previous_work_thread == Some(entry.id))
        })
        .map(|entry| entry.id);
    project.active_thread_id = match previous {
        Some(prior) if selected == Some(prior) => Some(thread),
        _ => selected,
    };
    if let Some(record) = project.threads.iter_mut().find(|entry| entry.id == thread) {
        record.started_by_dispatch = true;
        record.previous_work_thread = previous;
    }
}

fn no_change_reason(thread: &crate::ThreadItem) -> String {
    use velocity_core::thread_view_model::TranscriptBlock;

    thread
        .transcript
        .iter()
        .rev()
        .find_map(|block| match block {
            TranscriptBlock::AgentMessage { paragraphs } => {
                let message = paragraphs.join("\n\n");
                (!message.trim().is_empty()).then_some(message)
            }
            _ => None,
        })
        .unwrap_or_else(|| "The agent left no final message.".into())
}

impl StateStore {
    pub fn finish_empty_dispatch(
        &mut self,
        project: u32,
        identifier: u32,
    ) -> Result<(), ThreadError> {
        self.finish_empty_dispatch_with_nick(project, identifier, None)
    }

    pub fn finish_empty_dispatch_with_nick(
        &mut self,
        project: u32,
        identifier: u32,
        nick: Option<String>,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let thread = item
            .threads
            .iter_mut()
            .find(|thread| thread.id == identifier)
            .ok_or(ThreadError::UnknownThread {
                project,
                thread: identifier,
            })?;
        if !can_finish_empty_dispatch(thread) {
            return Err(ThreadError::InvalidLaunch(
                "The thread resumed or still has unread messages".into(),
            ));
        }
        thread.state = ThreadState::Done;
        let reason = no_change_reason(thread);
        thread.no_change_reason = Some(reason.clone());
        if thread.started_by_dispatch {
            if let (Some(issue), Some(change)) = (&thread.issue, &thread.rosen_change) {
                item.continuous_dispatch.no_change_reports.insert(
                    identifier,
                    NoChangeReport {
                        issue: issue.clone(),
                        change: change.clone(),
                        nick,
                        reason,
                    },
                );
            }
            for task in &mut item.tasks {
                if task.assigned_thread_id == Some(identifier) && task.state == TaskState::Now {
                    item.continuous_dispatch
                        .task_attempts
                        .insert(task.id, thread.title.clone());
                    thread.task_ids.retain(|listed| *listed != task.id);
                    task.assigned_thread_id = None;
                    task.state = TaskState::Todo;
                }
            }
        }
        if !item.continuous_dispatch.cleanup.contains(&identifier) {
            item.continuous_dispatch.cleanup.push(identifier);
        }
        crate::rosen_work::remove_thread(item, identifier);
        self.commit(next)?;
        Ok(())
    }

    pub fn finish_no_change_report(
        &mut self,
        project: u32,
        identifier: u32,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let dispatch = &mut next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?
            .continuous_dispatch;
        if let Some(report) = dispatch.no_change_reports.remove(&identifier) {
            // The GitHub label now owns eligibility. Removing it reopens dispatch
            // even if GitHub gives the issue the same updated_at timestamp.
            dispatch.attempts.retain(|attempt| {
                attempt.number != report.issue.number
                    || !attempt
                        .repository
                        .eq_ignore_ascii_case(&report.issue.repository)
            });
            self.commit(next)?;
        }
        Ok(())
    }

    /// A dispatch thread that failed before its agent's first turn did no work (#514). Its
    /// issue goes back to dispatch in the same save: the attempt is forgotten, so the
    /// failure does not count as one, and the failed thread, kept with its error, no
    /// longer owns the issue. `reason` waits in `launch_failure_reports` for its comment.
    /// Past [`UNSTARTED_RETURNS`] failures in a row dispatch waits before each further
    /// try and posts no more comments. `now` is Unix seconds.
    /// Returns whether the thread was such a failure; other threads are left untouched.
    pub fn return_unstarted_dispatch(
        &mut self,
        project: u32,
        identifier: u32,
        reason: &str,
        now: u64,
    ) -> Result<bool, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let Some(thread) = item
            .threads
            .iter_mut()
            .find(|thread| thread.id == identifier && failed_before_first_turn(thread))
        else {
            return Ok(false);
        };
        let Some(issue) = thread.issue.clone() else {
            return Ok(false);
        };
        thread.supervision.released = true;
        let dispatch = &mut item.continuous_dispatch;
        let failure = dispatch.count_unstarted_failure(&issue, now);
        if failure <= UNSTARTED_RETURNS {
            dispatch.launch_failure_reports.insert(
                identifier,
                LaunchFailureReport {
                    issue: issue.clone(),
                    reason: reason.to_owned(),
                    failure,
                },
            );
        }
        dispatch.attempts.retain(|attempt| {
            attempt.number != issue.number
                || !attempt.repository.eq_ignore_ascii_case(&issue.repository)
        });
        crate::rosen_work::remove_thread(item, identifier);
        self.commit(next)?;
        Ok(true)
    }

    /// Forgets failed starts of issues that left the open backlog, and of issues whose
    /// thread has since run a turn: that launch worked (#514).
    pub fn prune_unstarted_failures(
        &mut self,
        project: u32,
        repository: &str,
        open: impl Fn(u32) -> bool,
    ) -> Result<(), ThreadError> {
        let item = self
            .project(project)
            .ok_or(ThreadError::UnknownProject(project))?;
        // Only a thread started after the newest failure shows that launches work again.
        let started = |failures: &UnstartedFailures| {
            item.threads.iter().any(|thread| {
                thread.supervision.turns > 0
                    && thread.supervision.started >= failures.last
                    && thread
                        .issue
                        .as_ref()
                        .is_some_and(|assigned| same_issue(assigned, &failures.issue))
            })
        };
        let dropped: Vec<u32> = item
            .continuous_dispatch
            .unstarted_failures
            .iter()
            .filter(|(number, failures)| {
                failures.issue.repository.eq_ignore_ascii_case(repository)
                    && (!open(**number) || started(failures))
            })
            .map(|(number, _)| *number)
            .collect();
        if dropped.is_empty() {
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            item.continuous_dispatch
                .unstarted_failures
                .retain(|number, _| !dropped.contains(number));
        }
        self.commit(next)?;
        Ok(())
    }

    /// The failure's comment reached its issue, or its issue is gone.
    pub fn finish_launch_failure_report(
        &mut self,
        project: u32,
        identifier: u32,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let dispatch = &mut next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?
            .continuous_dispatch;
        if dispatch
            .launch_failure_reports
            .remove(&identifier)
            .is_some()
        {
            self.commit(next)?;
        }
        Ok(())
    }

    pub fn finish_dispatch_cleanup(
        &mut self,
        project: u32,
        identifier: u32,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        item.continuous_dispatch
            .cleanup
            .retain(|pending| *pending != identifier);
        self.commit(next)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentActivity, PersistedState, issues::ThreadIssue, threads::NewThread};

    // Risk: a restart must keep pause, issue ownership, attempts and pending cleanup,
    // or an empty result can dispatch the same issue again and strand its path claims.
    #[test]
    fn continuous_dispatch_survives_save_and_load() -> Result<(), Box<dyn std::error::Error>> {
        let (directory, mut store) = legacy_store()?;
        store.set_continuous_dispatch(1, 2, false)?;
        let thread = NewThread::new(
            "Issue".into(),
            "vel/issue-42".into(),
            directory.join("agent").display().to_string(),
            &store.settings(),
        );
        let issue = ThreadIssue::new("owner/repo".into(), 42);
        let identifier = store.record_continuous_dispatch(
            1,
            thread.clone(),
            issue.clone(),
            "version-1".into(),
        )?;
        let reason = "Computer Use failed. I could not verify the issue.";
        store.update_thread(1, identifier, |thread| {
            thread.activity = AgentActivity::Idle;
            thread.rosen_change = Some("empty-change".into());
            std::sync::Arc::make_mut(&mut thread.transcript).extend([
                velocity_core::thread_view_model::TranscriptBlock::AgentMessage {
                    paragraphs: vec!["I will check the issue.".into()],
                },
                velocity_core::thread_view_model::TranscriptBlock::AgentMessage {
                    paragraphs: vec![reason.into()],
                },
            ]);
        })?;
        store.finish_empty_dispatch_with_nick(1, identifier, Some("maple".into()))?;
        store.set_continuous_dispatch(1, 2, true)?;
        drop(store);
        let mut loaded = StateStore::load_or_default(&directory)?;
        let project = loaded.project(1).ok_or("missing project")?;
        assert_eq!(
            (
                project.continuous_dispatch.slots,
                project.continuous_dispatch.paused
            ),
            (2, true)
        );
        assert_eq!(project.continuous_dispatch.cleanup, vec![identifier]);
        assert_eq!(project.threads[0].state, ThreadState::Done);
        assert_eq!(project.active_thread_id, None);
        assert_eq!(project.threads[0].issue.as_ref(), Some(&issue));
        assert_eq!(project.threads[0].no_change_reason.as_deref(), Some(reason));
        let report = &project.continuous_dispatch.no_change_reports[&identifier];
        assert_eq!(report.issue, issue);
        assert_eq!(report.change, "empty-change");
        assert_eq!(report.reason, reason);
        assert_eq!(report.nick.as_deref(), Some("maple"));
        loaded.set_continuous_dispatch(1, 2, false)?;
        assert!(
            loaded
                .record_continuous_dispatch(1, thread.clone(), issue.clone(), "version-2".into())
                .is_err()
        );
        loaded.finish_no_change_report(1, identifier)?;
        loaded.record_continuous_dispatch(1, thread, issue, "version-1".into())?;
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#504): a landed issue whose thread history was deleted is sent to another
    // agent after a restart. The GitHub land hold and its first-seen time must persist.
    #[test]
    fn landed_hold_survives_save_and_load() -> Result<(), Box<dyn std::error::Error>> {
        let (directory, mut store) = legacy_store()?;
        let issue = ThreadIssue::new("Owner/Repo".into(), 410);
        let commit = "c".repeat(40);
        store.save_landed_hold(1, issue.clone(), commit.clone(), "v1".into(), 100)?;
        store.save_landed_hold(1, issue.clone(), commit.clone(), "v2".into(), 900)?;
        drop(store);
        let mut loaded = StateStore::load_or_default(&directory)?;
        let project = loaded.project(1).ok_or("missing project")?;
        let lookup = ThreadIssue::new("owner/repo".into(), 410);
        let wait = project.landed_awaiting_integration(&lookup, Some("v2"));
        assert_eq!(
            wait,
            Some(LandedWait {
                commit: Some(commit),
                since: Some(100)
            })
        );
        assert!(
            project
                .landed_awaiting_integration(&lookup, Some("v3"))
                .is_none()
        );
        loaded.prune_landed_holds(1, "owner/repo", |number| number != 410)?;
        drop(loaded);
        let reloaded = StateStore::load_or_default(&directory)?;
        let project = reloaded.project(1).ok_or("missing project")?;
        assert!(project.continuous_dispatch.landed_holds.is_empty());
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#525): a restart that forgets an ended overlap hold lets dispatch hold the
    // same issue again behind the next running issue, so it never starts. The hold's
    // overlapping files and the release must persist together.
    #[test]
    fn overlap_release_survives_save_and_load() -> Result<(), Box<dyn std::error::Error>> {
        let (directory, mut store) = legacy_store()?;
        let held = ThreadIssue::new("Owner/Repo".into(), 126);
        let returned = ThreadIssue::new("Owner/Repo".into(), 133);
        let wait = IssueOverlap::new(held.clone(), 109, Some(7), vec!["web/explore.tsx".into()]);
        store.save_issue_overlaps(
            1,
            BTreeMap::from([(126, wait.clone())]),
            BTreeMap::from([(133, returned.clone())]),
        )?;
        drop(store);
        let mut loaded = StateStore::load_or_default(&directory)?;
        let project = loaded.project(1).ok_or("missing project")?;
        let lookup = ThreadIssue::new("owner/repo".into(), 126);
        assert_eq!(project.overlap_hold(&lookup), Some(&wait));
        assert_eq!(
            project.continuous_dispatch.overlap_released,
            BTreeMap::from([(133, returned.clone())])
        );
        loaded.save_issue_overlaps(
            1,
            BTreeMap::new(),
            BTreeMap::from([(126, held.clone()), (133, returned.clone())]),
        )?;
        drop(loaded);
        let reloaded = StateStore::load_or_default(&directory)?;
        let project = reloaded.project(1).ok_or("missing project")?;
        assert!(project.overlap_hold(&lookup).is_none());
        assert_eq!(
            project.continuous_dispatch.overlap_released,
            BTreeMap::from([(126, held), (133, returned)])
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#514): a restart between a failed start and its comment must not lose the
    // reason, re-own the issue for the failed thread, or forget how often the launch
    // failed, which is what stops a launch that cannot succeed from starting for ever.
    #[test]
    fn unstarted_failure_survives_save_and_load() -> Result<(), Box<dyn std::error::Error>> {
        let (directory, mut store) = legacy_store()?;
        store.set_continuous_dispatch(1, 2, false)?;
        let issue = ThreadIssue::new("Owner/Repo".into(), 421);
        let mut identifier = 0;
        for failure in 1..=UNSTARTED_RETURNS + 1 {
            let thread = NewThread::new(
                "Issue".into(),
                format!("vel/issue-421-{failure}"),
                directory
                    .join(format!("agent-{failure}"))
                    .display()
                    .to_string(),
                &store.settings(),
            );
            // The same issue version every time: a returned start is no attempt.
            identifier = store.record_continuous_dispatch(1, thread, issue.clone(), "v1".into())?;
            assert!(!store.return_unstarted_dispatch(1, identifier, "still working", 100)?);
            store.update_thread(1, identifier, |thread| thread.state = ThreadState::Error)?;
            assert!(store.return_unstarted_dispatch(1, identifier, "pipe busy", 100)?);
        }
        drop(store);
        let mut loaded = StateStore::load_or_default(&directory)?;
        let project = loaded.project(1).ok_or("missing project")?;
        let dispatch = &project.continuous_dispatch;
        assert!(dispatch.attempts.is_empty());
        assert!(crate::issues::issue_thread(project, &issue).is_none());
        // Failures past the limit post no comment and hold the issue for an hour.
        assert_eq!(dispatch.launch_failure_reports.len(), 3);
        assert!(!dispatch.launch_failure_reports.contains_key(&identifier));
        assert_eq!(dispatch.launch_holds(100 + 3599).count(), 1);
        assert_eq!(dispatch.launch_holds(100 + 3600).count(), 0);
        let first = *dispatch
            .launch_failure_reports
            .keys()
            .next()
            .ok_or("missing report")?;
        assert_eq!(dispatch.launch_failure_reports[&first].reason, "pipe busy");
        loaded.finish_launch_failure_report(1, first)?;
        loaded.prune_unstarted_failures(1, "owner/repo", |_| false)?;
        drop(loaded);
        let reloaded = StateStore::load_or_default(&directory)?;
        let dispatch = &reloaded
            .project(1)
            .ok_or("missing project")?
            .continuous_dispatch;
        assert_eq!(dispatch.launch_failure_reports.len(), 2);
        assert!(dispatch.unstarted_failures.is_empty());
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    fn legacy_store() -> Result<(std::path::PathBuf, StateStore), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "velocity-dispatch-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory)?;
        let mut project = ProjectItem::new(1, "dispatch".into(), directory.display().to_string());
        project.spec_locked = true;
        let mut state = PersistedState::default();
        state.projects.push(project);
        let mut legacy = serde_json::to_value(state)?;
        legacy["projects"][0]
            .as_object_mut()
            .ok_or("missing project")?
            .remove("continuous_dispatch");
        std::fs::write(directory.join("state.json"), serde_json::to_vec(&legacy)?)?;
        let store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store
                .project(1)
                .ok_or("missing project")?
                .continuous_dispatch
                .slots,
            0
        );
        Ok((directory, store))
    }
}
