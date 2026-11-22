//! Work ownership and lifecycle saved with the Rosen event cursor.

use velocity_core::thread_view_model::RosenChangeStage;
use velocity_core::work_pool::{WorkItem, WorkKey, WorkStatus};

use crate::issues::ThreadIssue;
use crate::rosen::RosenProgress;
use crate::threads::{ThreadError, thread_start_timestamp};
use crate::{ProjectItem, StateStore, StoreError, TaskState, ThreadItem, ThreadState};

/// Commit and diagnostic evidence carried by a Rosen land or revert event.
/// `commit` holds the landed commit for `Landed` and the reverted culprit commit for `Reverted`.
#[derive(Clone, Copy, Default)]
pub struct WorkEvidence<'a> {
    pub commit: Option<&'a str>,
    pub revert_reason: Option<&'a str>,
}

/// Open-issue timestamps and optional title/body used to check whether a completed Rosen
/// work item still covers the current host issue (#602, #614).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IssueVersion<'a> {
    pub updated_at: Option<&'a str>,
    pub reopened_at: Option<&'a str>,
    pub title: Option<&'a str>,
    pub body: Option<&'a str>,
}

impl<'a> IssueVersion<'a> {
    pub fn from_github(issue: &'a velocity_github::GitHubIssue) -> Self {
        Self {
            updated_at: Some(issue.updated_at.as_str()),
            reopened_at: issue.reopened_at.as_deref(),
            title: Some(issue.title.as_str()),
            body: Some(issue.body.as_str()),
        }
    }

    pub fn updated_at(updated_at: &'a str) -> Self {
        Self {
            updated_at: Some(updated_at),
            reopened_at: None,
            title: None,
            body: None,
        }
    }
}

/// Checks whether a host issue described by `version` matches a landed work item's expected
/// title or GitHub source (#614). Returns `true` when `version` carries no title/body or
/// when the title or GitHub source matches.
pub(crate) fn host_issue_matches_work_title(
    expected_title: Option<&str>,
    repository: &str,
    number: u32,
    version: IssueVersion<'_>,
) -> bool {
    let Some(host_title) = version.title else {
        return true;
    };
    let expected = velocity_core::collapse::ExpectedIssueIdentity {
        title: expected_title,
        github_repository: Some(repository),
        github_number: Some(number),
    };
    velocity_core::collapse::host_issue_matches_expected(
        &expected,
        host_title,
        version.body.unwrap_or_default(),
    )
}

/// Parses either a decimal Unix-seconds string or an RFC3339 timestamp into UTC Unix seconds.
pub fn parse_timestamp_seconds(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return trimmed.parse::<u64>().ok();
    }
    let (date, time_and_zone) = trimmed.split_once(['T', 't'])?;
    let mut date_parts = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (
        date_parts.next()??,
        date_parts.next()??,
        date_parts.next()??,
    );
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (time_part, offset_seconds) = split_rfc3339_offset(time_and_zone)?;
    let clock = time_part.split('.').next()?;
    let mut clock_parts = clock.split(':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (
        clock_parts.next()??,
        clock_parts.next()??,
        clock_parts.next()??,
    );
    if clock_parts.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let year_of_era = adjusted_year.rem_euclid(400);
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = adjusted_year.div_euclid(400) * 146_097 + day_of_era - 719_468;
    let utc_seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_seconds;
    u64::try_from(utc_seconds).ok()
}

fn split_rfc3339_offset(time_and_zone: &str) -> Option<(&str, i64)> {
    if let Some(clock) = time_and_zone
        .strip_suffix('Z')
        .or_else(|| time_and_zone.strip_suffix('z'))
    {
        return Some((clock, 0));
    }
    let sign_index = time_and_zone.rfind(['+', '-'])?;
    let (clock, zone) = time_and_zone.split_at(sign_index);
    let sign = if zone.starts_with('+') { 1 } else { -1 };
    let (hours, minutes) = zone[1..].split_once(':')?;
    let hours = hours.parse::<i64>().ok()?;
    let minutes = minutes.parse::<i64>().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some((clock, sign * (hours * 3_600 + minutes * 60)))
}

fn completed_timestamp(item: &WorkItem) -> Option<u64> {
    let raw = match item.status {
        WorkStatus::Green => item
            .green_at
            .as_deref()
            .or(item.landed_at.as_deref())
            .or(item.updated_at.as_deref()),
        WorkStatus::Landed => item.landed_at.as_deref().or(item.updated_at.as_deref()),
        WorkStatus::Claimed | WorkStatus::InChange | WorkStatus::Reopened => return None,
    };
    raw.and_then(parse_timestamp_seconds)
}

/// Whether a completed work item is superseded by a newer open version of its issue (#602).
///
/// For `Green` items, either a reopen event or a newer `updated_at` timestamp after
/// `green_at`/`landed_at` makes the issue actionable again. For `Landed` items still
/// awaiting integration, Velocity's own land comment bumps `updated_at` right after
/// landing, so only a reopen event later than `landed_at` supersedes the in-flight land.
pub(crate) fn item_superseded_by_issue(item: &WorkItem, version: IssueVersion<'_>) -> bool {
    if !item.status.completed() {
        return false;
    }
    let completed_at = completed_timestamp(item);
    if let Some(reopened_at) = version.reopened_at.and_then(parse_timestamp_seconds)
        && completed_at.is_none_or(|done| reopened_at > done)
    {
        return true;
    }
    item.status == WorkStatus::Green
        && matches!(
            (
                version.updated_at.and_then(parse_timestamp_seconds),
                completed_at,
            ),
            (Some(updated_at), Some(done)) if updated_at > done
        )
}

impl ProjectItem {
    /// Returns the newest tracked Rosen work item for `key`. Earlier completed items for
    /// reopened issues remain in `rosen_work_items` for history (#602).
    pub fn rosen_work_item(&self, key: &WorkKey) -> Option<&WorkItem> {
        self.rosen_work_items
            .iter()
            .rev()
            .find(|item| &item.key == key)
    }

    pub fn rosen_issue_completed(&self, issue: &ThreadIssue) -> bool {
        self.rosen_issue_completed_for_version(issue, IssueVersion::default())
    }

    /// Whether `issue` is covered by a completed Rosen work item at `version` (#602, #614).
    /// Returns `false` when an open issue's `updated_at` or reopen event is later than the
    /// work item's completion timestamp, or when a number-colliding host issue has a
    /// different title and GitHub source.
    pub fn rosen_issue_completed_for_version(
        &self,
        issue: &ThreadIssue,
        version: IssueVersion<'_>,
    ) -> bool {
        let key = WorkKey::issue(&issue.repository, issue.number);
        if let Some(item) = self.rosen_work_item(&key) {
            return item.status.completed()
                && !item_superseded_by_issue(item, version)
                && host_issue_matches_work_title(
                    Some(&item.title),
                    &issue.repository,
                    issue.number,
                    version,
                );
        }
        if version.reopened_at.is_some() {
            return false;
        }
        self.completed_goal_issues.iter().any(|completed| {
            same_issue(completed, issue)
                && host_issue_matches_work_title(
                    completed.title.as_deref(),
                    &issue.repository,
                    issue.number,
                    version,
                )
        }) || self.threads.iter().any(|thread| {
            thread.rosen_change.is_some()
                && thread.state == ThreadState::Merged
                && thread.issue.as_ref().is_some_and(|assigned| {
                    same_issue(assigned, issue)
                        && host_issue_matches_work_title(
                            assigned.title.as_deref().or(Some(thread.title.as_str())),
                            &issue.repository,
                            issue.number,
                            version,
                        )
                })
        })
    }

    /// Whether `issue` has a landed Rosen change covered by a green integration build (#529).
    /// A land waiting for its integration build, or a reverted land, returns `false`.
    pub fn rosen_issue_integration_green(&self, issue: &ThreadIssue) -> bool {
        let key = WorkKey::issue(&issue.repository, issue.number);
        if let Some(item) = self.rosen_work_item(&key) {
            return match item.status {
                WorkStatus::Green => true,
                WorkStatus::Landed => {
                    self.issue_green_for_commit(issue, item.landed_commit.as_deref())
                }
                WorkStatus::Claimed | WorkStatus::InChange | WorkStatus::Reopened => false,
            };
        }
        self.issue_green_for_commit(issue, None)
    }

    fn issue_green_for_commit(&self, issue: &ThreadIssue, commit: Option<&str>) -> bool {
        let thread_green = self.threads.iter().any(|thread| {
            thread
                .issue
                .as_ref()
                .is_some_and(|assigned| same_issue(assigned, issue))
                && thread.rosen_issue.is_integration_green()
                && commit.is_none_or(|sha| thread.rosen_issue.landed_commit() == Some(sha))
        });
        let retained_green = self.retained_rosen_issues.values().any(|retained| {
            same_issue(&retained.issue, issue)
                && retained.progress.is_integration_green()
                && commit.is_none_or(|sha| retained.progress.landed_commit() == Some(sha))
        });
        thread_green
            || retained_green
            || self
                .continuous_dispatch
                .landed_holds
                .get(&issue.number)
                .is_some_and(|hold| {
                    same_issue(&hold.issue, issue)
                        && hold.green_since.is_some()
                        && commit.is_none_or(|sha| hold.commit == sha)
                })
    }

    pub fn rosen_source_items(&self, identifier: u32) -> Vec<WorkKey> {
        self.threads
            .iter()
            .find(|thread| thread.id == identifier)
            .map(|thread| {
                sources(self, thread)
                    .into_iter()
                    .map(|(key, _)| key)
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn same_issue(left: &ThreadIssue, right: &ThreadIssue) -> bool {
    left.number == right.number && left.repository.eq_ignore_ascii_case(&right.repository)
}

fn sources(project: &ProjectItem, thread: &ThreadItem) -> Vec<(WorkKey, String)> {
    let mut items: Vec<_> = project
        .tasks
        .iter()
        .filter(|task| {
            task.assigned_thread_id == Some(thread.id)
                || (task.assigned_thread_id.is_none() && thread.task_ids.contains(&task.id))
        })
        .map(|task| {
            (
                WorkKey::Task {
                    identifier: task.id,
                },
                task.text.clone(),
            )
        })
        .collect();
    if let Some(issue) = &thread.issue {
        items.push((
            WorkKey::issue(&issue.repository, issue.number),
            thread.title.clone(),
        ));
    }
    items
}

fn track(project: &mut ProjectItem, identifier: u32, at: Option<&str>, restore: bool) {
    let Some(thread) = project
        .threads
        .iter()
        .find(|thread| thread.id == identifier)
    else {
        return;
    };
    if (thread.rosen_change.is_none() && project.version_control != crate::VersionControl::Rosen)
        || thread.supervision.released
        || thread.state == ThreadState::Done
        || (thread.state == ThreadState::Merged && thread.rosen_change.is_none())
    {
        return;
    }
    for (key, title) in sources(project, thread) {
        if update_owned_work_item(
            &mut project.rosen_work_items,
            &key,
            identifier,
            thread,
            at,
            restore,
        ) {
            continue;
        }
        let latest = project
            .rosen_work_items
            .iter_mut()
            .rev()
            .find(|item| item.key == key);
        match latest {
            Some(item) if !restore && !item.status.completed() => {
                *item = claimed(key, title, thread, at);
            }
            // Re-dispatching a reopened issue keeps its prior completed work item for history
            // and tracks the new Rosen change as a fresh work item (#602).
            Some(item)
                if item.status.completed() && (!restore || thread.state != ThreadState::Merged) =>
            {
                let mut next = claimed(key, title, thread, at);
                if restore {
                    restore_status(&mut next, thread);
                }
                project.rosen_work_items.push(next);
            }
            Some(_) => {}
            None => {
                let mut item = claimed(key, title, thread, at);
                if restore {
                    restore_status(&mut item, thread);
                }
                project.rosen_work_items.push(item);
            }
        }
    }
}

fn update_owned_work_item(
    work_items: &mut [WorkItem],
    key: &WorkKey,
    identifier: u32,
    thread: &ThreadItem,
    at: Option<&str>,
    restore: bool,
) -> bool {
    let Some(item) = work_items.iter_mut().rev().find(|item| {
        &item.key == key
            && (item.thread_identifier == Some(identifier)
                || (restore && item.change.is_some() && item.change == thread.rosen_change))
    }) else {
        return false;
    };
    if item.thread_identifier.is_none() && restore {
        item.thread_identifier = Some(identifier);
    }
    if item.change != thread.rosen_change {
        item.change.clone_from(&thread.rosen_change);
        item.status = WorkStatus::Claimed;
        item.updated_at = at.map(str::to_owned);
        item.landed_at = None;
        item.green_at = None;
        item.landed_commit = None;
    } else if restore
        && item.status == WorkStatus::Landed
        && item.landed_at.is_none()
        && item.landed_commit.is_none()
    {
        item.landed_commit = saved_land(thread);
    }
    true
}

fn claimed(key: WorkKey, title: String, thread: &ThreadItem, at: Option<&str>) -> WorkItem {
    WorkItem {
        key,
        title,
        thread_identifier: Some(thread.id),
        change: thread.rosen_change.clone(),
        status: WorkStatus::Claimed,
        claimed_at: at.map(str::to_owned),
        updated_at: at.map(str::to_owned),
        landed_at: None,
        green_at: None,
        reverted_at: None,
        landed_commit: None,
        revert_reason: None,
    }
}

fn restore_status(item: &mut WorkItem, thread: &ThreadItem) {
    let view = thread
        .rosen_change_view
        .as_ref()
        .filter(|view| Some(&view.identifier) == item.change.as_ref());
    if thread.state == ThreadState::Merged {
        item.status = WorkStatus::Landed;
        item.landed_commit = saved_land(thread);
    } else if view.is_some_and(|view| view.stage == RosenChangeStage::Reopened)
        || thread.rosen_issue.was_reverted()
    {
        item.status = WorkStatus::Reopened;
    } else if item.change.is_some()
        && (thread.review_stage.is_some() || view.is_some_and(|view| view.name.is_some()))
    {
        item.status = WorkStatus::InChange;
    }
}

fn saved_land(thread: &ThreadItem) -> Option<String> {
    thread
        .rosen_issue
        .landed_commit()
        .map(str::to_owned)
        .or_else(|| {
            let view = thread.rosen_change_view.as_ref()?;
            if thread.rosen_change.as_ref() != Some(&view.identifier) {
                return None;
            }
            let land = view.lands.last().filter(|land| land.revert.is_none())?;
            Some(land.commit.clone())
        })
}

pub(crate) fn track_claim(project: &mut ProjectItem, identifier: u32) {
    track(project, identifier, Some(&thread_start_timestamp()), false);
}

pub(crate) fn restore_thread(project: &mut ProjectItem, identifier: u32) {
    track(project, identifier, None, true);
}

/// Closing/deleting history releases active work and retains landed completion.
pub(crate) fn remove_thread(project: &mut ProjectItem, identifier: u32) {
    project.rosen_work_items.retain_mut(|item| {
        if item.thread_identifier != Some(identifier) {
            return true;
        }
        item.thread_identifier = None;
        item.status.completed()
    });
}

pub(crate) fn apply(
    project: &mut ProjectItem,
    change: &str,
    progress: RosenProgress,
    evidence: WorkEvidence<'_>,
) {
    if progress == RosenProgress::ChangesRequested {
        return;
    }
    let at = thread_start_timestamp();
    if progress == RosenProgress::Abandoned {
        project.rosen_work_items.retain(|item| {
            if item.change.as_deref() != Some(change) || item.status.completed() {
                return true;
            }
            if let WorkKey::Issue { repository, number } = &item.key {
                // Empty dispatch already retired its projection and keeps its no-change hold.
                project.continuous_dispatch.attempts.retain(|attempt| {
                    attempt.number != *number
                        || !attempt.repository.eq_ignore_ascii_case(repository)
                });
            }
            false
        });
        return;
    }
    for item in &mut project.rosen_work_items {
        if item.change.as_deref() != Some(change) {
            continue;
        }
        // After a cursor rewind, Rosen can skip a superseded revert and replay only
        // the latest land. Its commit distinguishes that land from the saved one.
        let replaced_land = progress == RosenProgress::Landed
            && evidence
                .commit
                .zip(item.landed_commit.as_deref())
                .is_some_and(|(observed, saved)| observed != saved);
        let status = match progress {
            RosenProgress::Named
                if matches!(item.status, WorkStatus::Claimed | WorkStatus::Reopened) =>
            {
                WorkStatus::InChange
            }
            RosenProgress::Landed if !item.status.completed() || replaced_land => {
                WorkStatus::Landed
            }
            RosenProgress::Reverted if item.status != WorkStatus::Reopened => WorkStatus::Reopened,
            _ => item.status,
        };
        let new_land = progress == RosenProgress::Landed
            && (status != item.status
                || replaced_land
                || (item.landed_commit.is_none() && evidence.commit.is_some()));
        if new_land {
            if replaced_land {
                item.landed_at = None;
                item.green_at = None;
            }
            item.landed_at.get_or_insert_with(|| at.clone());
            item.landed_commit = evidence.commit.map(str::to_owned);
        }
        if status == item.status && !replaced_land {
            continue;
        }
        item.status = status;
        item.updated_at = Some(at.clone());
        if status == WorkStatus::Reopened {
            reopen(
                item,
                &mut project.tasks,
                &mut project.completed_goal_issues,
                &at,
                evidence.revert_reason,
            );
        }
    }
}

fn reopen(
    item: &mut WorkItem,
    tasks: &mut [crate::TaskItem],
    completed_issues: &mut Vec<ThreadIssue>,
    at: &str,
    reason: Option<&str>,
) {
    item.landed_at = None;
    item.green_at = None;
    item.landed_commit = None;
    item.reverted_at = Some(at.to_owned());
    item.revert_reason = reason.map(str::to_owned);
    match &item.key {
        WorkKey::Issue { repository, number } => {
            // A deleted completing thread can no longer clear the legacy goal receipt.
            completed_issues.retain(|issue| {
                issue.number != *number || !issue.repository.eq_ignore_ascii_case(repository)
            });
        }
        WorkKey::Task { identifier } if item.thread_identifier.is_none() => {
            if let Some(task) = tasks.iter_mut().find(|task| task.id == *identifier)
                && task.state == TaskState::Done
            {
                task.state = TaskState::Todo;
            }
        }
        _ => {}
    }
}

impl StateStore {
    pub fn update_rosen_work_thread(
        &mut self,
        project: u32,
        thread: u32,
        update: impl FnOnce(&mut ThreadItem),
    ) -> Result<bool, ThreadError> {
        self.try_edit_project(
            project,
            || ThreadError::UnknownProject(project),
            |item| {
                let record = item
                    .threads
                    .iter_mut()
                    .find(|item| item.id == thread)
                    .ok_or(ThreadError::UnknownThread { project, thread })?;
                let before = record.clone();
                update(record);
                let changed = *record != before;
                let before_work = item.rosen_work_items.clone();
                track_claim(item, thread);
                Ok((changed || item.rosen_work_items != before_work, changed))
            },
        )
    }

    /// Seed records saved before the projection existed. The existing cursor remains valid.
    pub fn restore_rosen_work(&mut self, project: u32) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |item| {
                let before = item.rosen_work_items.clone();
                let threads: Vec<_> = item.threads.iter().map(|thread| thread.id).collect();
                for thread in threads {
                    track(item, thread, None, true);
                }
                let changed = item.rosen_work_items != before;
                (changed, changed)
            })?
            .unwrap_or(false))
    }

    pub fn confirm_rosen_work_build(
        &mut self,
        project: u32,
        sequence: u64,
        covered: &[String],
    ) -> Result<bool, StoreError> {
        Ok(self
            .edit_project_in_place(project, |item| {
                if sequence != 0 && sequence <= item.rosen_event_cursor {
                    return (false, false);
                }
                let mut changed = false;
                let at = thread_start_timestamp();
                for work in &mut item.rosen_work_items {
                    if work.status == WorkStatus::Landed
                        && work
                            .landed_commit
                            .as_ref()
                            .is_some_and(|commit| covered.contains(commit))
                    {
                        work.status = WorkStatus::Green;
                        work.updated_at = Some(at.clone());
                        work.green_at = Some(at.clone());
                        changed = true;
                    }
                }
                let dirty = changed || sequence > item.rosen_event_cursor;
                if dirty {
                    item.rosen_event_cursor = item.rosen_event_cursor.max(sequence);
                }
                (dirty, changed)
            })?
            .unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TaskAttribution, TaskItem, VersionControl};
    use std::path::{Path, PathBuf};

    #[test]
    fn task_and_issue_follow_the_same_land_and_revert_across_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        // Saving a cursor without the corresponding work transition would lose the item
        // state permanently on reconnect. A test-only green event carries no build coverage.
        let (directory, mut store) = fixture()?;
        assert_items(&store, WorkStatus::Claimed, "original", Some(4));
        event(&mut store, 1, "original", RosenProgress::Named, None)?;
        assert_items(&store, WorkStatus::InChange, "original", Some(4));
        event(
            &mut store,
            2,
            "original",
            RosenProgress::Landed,
            Some("first"),
        )?;
        let issue = ThreadIssue::new("Owner/Repo".into(), 7);
        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.rosen_event_cursor(1), 2);
        assert_items(&store, WorkStatus::Landed, "original", Some(4));
        assert!(!store.state.projects[0].rosen_issue_integration_green(&issue));
        assert!(!store.confirm_rosen_work_build(1, 3, &[])?);
        assert_items(&store, WorkStatus::Landed, "original", Some(4));
        assert!(!store.state.projects[0].rosen_issue_integration_green(&issue));
        assert!(store.confirm_rosen_work_build(1, 4, &["first".into()])?);
        assert_items(&store, WorkStatus::Green, "original", Some(4));
        assert!(store.state.projects[0].rosen_issue_integration_green(&issue));
        event(&mut store, 5, "original", RosenProgress::Reverted, None)?;
        let mut store = StateStore::load_or_default(&directory)?;
        assert!(!store.state.projects[0].rosen_issue_integration_green(&issue));
        event(
            &mut store,
            2,
            "original",
            RosenProgress::Landed,
            Some("first"),
        )?;
        assert!(!store.confirm_rosen_work_build(1, 4, &["first".into()])?);
        assert_eq!(store.rosen_event_cursor(1), 5);
        assert_items(&store, WorkStatus::Reopened, "original", Some(4));
        assert_eq!(store.state.projects[0].tasks[0].state, TaskState::Now);
        event(&mut store, 6, "original", RosenProgress::Named, None)?;
        event(
            &mut store,
            7,
            "original",
            RosenProgress::Landed,
            Some("second"),
        )?;
        assert!(!store.state.projects[0].rosen_issue_integration_green(&issue));
        assert!(!store.confirm_rosen_work_build(1, 8, &["first".into()])?);
        assert!(store.confirm_rosen_work_build(1, 9, &["second".into()])?);
        store.remove_thread(1, 4)?;
        let store = StateStore::load_or_default(&directory)?;
        assert_items(&store, WorkStatus::Green, "original", None);
        assert!(store.state.projects[0].rosen_issue_completed(&issue));
        assert!(store.state.projects[0].rosen_issue_integration_green(&issue));
        assert_eq!(store.state.projects[0].tasks[0].state, TaskState::Done);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn abandonment_only_releases_the_current_change() -> Result<(), Box<dyn std::error::Error>> {
        // A predecessor can be abandoned after the replacement reaches disk.
        let (directory, mut store) = fixture()?;
        event(&mut store, 1, "original", RosenProgress::Named, None)?;
        store.update_rosen_work_thread(1, 4, |thread| {
            thread.rosen_change = Some("stacked".into());
        })?;
        let mut store = StateStore::load_or_default(&directory)?;
        event(&mut store, 2, "original", RosenProgress::Abandoned, None)?;
        assert_items(&store, WorkStatus::Claimed, "stacked", Some(4));
        event(&mut store, 3, "stacked", RosenProgress::Abandoned, None)?;
        let store = StateStore::load_or_default(&directory)?;
        let project = &store.state.projects[0];
        assert!(project.rosen_work_items.is_empty());
        assert!(
            !project
                .continuous_dispatch
                .attempted("owner/repo", 7, "unchanged")
        );
        assert_eq!(project.tasks[0].state, TaskState::Todo);
        assert!(project.tasks[0].assigned_thread_id.is_none());
        assert!(!project.rosen_issue_completed(&ThreadIssue::new("owner/repo".into(), 7)));
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn recovered_cursor_takes_the_latest_land_commit() -> Result<(), Box<dyn std::error::Error>> {
        // After a database rebuild, the latest land can supersede the older revert.
        // The latest land must use its own build coverage.
        let (directory, mut store) = fixture()?;
        store.resume_rosen_events(1, "before rebuild", 0)?;
        event(
            &mut store,
            1,
            "original",
            RosenProgress::Landed,
            Some("first"),
        )?;
        assert!(store.confirm_rosen_work_build(1, 2, &["first".into()])?);
        let mut store = StateStore::load_or_default(&directory)?;
        store.resume_rosen_events(1, "after rebuild", 1)?;
        event(
            &mut store,
            1,
            "original",
            RosenProgress::Landed,
            Some("second"),
        )?;
        assert_items(&store, WorkStatus::Landed, "original", Some(4));
        assert!(!store.confirm_rosen_work_build(1, 2, &["first".into()])?);
        assert!(store.confirm_rosen_work_build(1, 3, &["second".into()])?);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#529): an orphaned landed hold or legacy thread must stay blocking until
    // integration confirms its commit, and remain satisfied across a restart once green.
    #[test]
    fn integration_green_blocker_state_survives_save_and_load()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, mut store) = fixture()?;
        let orphaned = ThreadIssue::new("Owner/Repo".into(), 133);
        let commit = "c".repeat(40);
        store.save_landed_hold(1, orphaned.clone(), commit.clone(), "v1".into(), 100)?;
        let mut loaded = StateStore::load_or_default(&directory)?;
        assert!(!loaded.state.projects[0].rosen_issue_integration_green(&orphaned));
        loaded.confirm_landed_hold_build(1, 133, &commit, 200)?;
        let loaded = StateStore::load_or_default(&directory)?;
        assert!(loaded.state.projects[0].rosen_issue_integration_green(&orphaned));
        assert!(
            !loaded.state.projects[0]
                .rosen_issue_integration_green(&ThreadIssue::new("Other/Repo".into(), 133))
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#602): a persisted completed Rosen work item must survive save and load for
    // history while a reopened issue is actionable again and gets a new work item.
    #[test]
    fn reopened_issue_keeps_completed_history_across_save_and_load()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, mut store) = fixture()?;
        store.set_continuous_dispatch(1, 2, false)?;
        let issue = ThreadIssue::new("Owner/Repo".into(), 7);
        let key = WorkKey::issue("owner/repo", 7);
        event(
            &mut store,
            1,
            "original",
            RosenProgress::Landed,
            Some("4a63d3f"),
        )?;
        assert!(store.confirm_rosen_work_build(1, 2, &["4a63d3f".into()])?);
        store.remove_thread(1, 4)?;
        let nov_18 = parse_timestamp_seconds("2026-11-18T12:00:00Z")
            .ok_or("valid timestamp")?
            .to_string();
        let mut snapshot = store.state.clone();
        if let Some(work) = snapshot.projects[0]
            .rosen_work_items
            .iter_mut()
            .find(|item| item.key == key)
        {
            work.landed_at = Some(nov_18.clone());
            work.green_at = Some(nov_18);
        }
        store.store_snapshot(snapshot)?;

        let mut loaded = StateStore::load_or_default(&directory)?;
        let same_version = IssueVersion::updated_at("2026-11-18T11:30:00Z");
        let reopened_updated = IssueVersion::updated_at("2026-11-21T10:00:00Z");
        let reopened_event = IssueVersion {
            updated_at: Some("2026-11-21T10:00:00Z"),
            reopened_at: Some("2026-11-21T10:00:00Z"),
            ..IssueVersion::default()
        };
        let colliding_issue = IssueVersion {
            updated_at: Some("2026-11-18T11:30:00Z"),
            reopened_at: None,
            title: Some("Unrelated Collapse report that collided on the same number"),
            body: Some("Filed natively on Collapse"),
        };
        assert!(loaded.state.projects[0].rosen_issue_completed_for_version(&issue, same_version));
        assert!(
            !loaded.state.projects[0].rosen_issue_completed_for_version(&issue, colliding_issue)
        );
        assert!(
            !loaded.state.projects[0].rosen_issue_completed_for_version(&issue, reopened_updated)
        );
        assert!(
            !loaded.state.projects[0].rosen_issue_completed_for_version(&issue, reopened_event)
        );

        let new_thread = crate::threads::NewThread::new(
            "Reopened issue #7".into(),
            "vel/issue-7".into(),
            r"C:\project\issue-7".into(),
            &loaded.settings(),
        );
        assert!(matches!(
            loaded.record_continuous_dispatch_for_version(
                1,
                new_thread.clone(),
                issue.clone(),
                "2026-11-18T11:30:00Z".into(),
                None,
            ),
            Err(ThreadError::IssueCompleted(7))
        ));
        let second_id = loaded.record_continuous_dispatch_for_version(
            1,
            new_thread,
            issue,
            "2026-11-21T10:00:00Z".into(),
            Some("2026-11-21T10:00:00Z"),
        )?;
        loaded.update_rosen_work_thread(1, second_id, |thread| {
            thread.rosen_change = Some("second-pass".into());
        })?;

        let reloaded = StateStore::load_or_default(&directory)?;
        let history: Vec<&WorkItem> = reloaded.state.projects[0]
            .rosen_work_items
            .iter()
            .filter(|item| item.key == key)
            .collect();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].status, WorkStatus::Green);
        assert_eq!(history[0].change.as_deref(), Some("original"));
        assert_eq!(history[0].landed_commit.as_deref(), Some("4a63d3f"));
        assert_eq!(history[1].status, WorkStatus::Claimed);
        assert_eq!(history[1].change.as_deref(), Some("second-pass"));
        assert_eq!(history[1].thread_identifier, Some(second_id));
        assert_eq!(
            reloaded.state.projects[0]
                .rosen_work_item(&key)
                .and_then(|item| item.change.as_deref()),
            Some("second-pass")
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    fn fixture() -> Result<(PathBuf, StateStore), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-rosen-work-{unique}"));
        Ok((directory.clone(), seed(&directory)?))
    }

    fn seed(directory: &Path) -> Result<StateStore, Box<dyn std::error::Error>> {
        let mut store = StateStore::load_or_default(directory)?;
        let mut project = ProjectItem::new(1, "Project".into(), r"C:\project".into());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        project
            .continuous_dispatch
            .attempts
            .push(crate::continuous_dispatch::DispatchAttempt {
                repository: "Owner/Repo".into(),
                number: 7,
                updated_at: "unchanged".into(),
            });
        let mut thread = ThreadItem::new(4, "Fix issue".into());
        thread.state = ThreadState::Working;
        thread.issue = Some(ThreadIssue::new("Owner/Repo".into(), 7));
        project.threads.push(thread);
        for number in [1, 2] {
            project.tasks.push(TaskItem::new(
                number,
                format!("Task {number}"),
                TaskState::Todo,
                TaskAttribution::User,
            ));
            crate::swarms::claim_task(&mut project, number, 4)?;
        }
        let mut next = store.state.clone();
        next.projects.push(project);
        store.store_snapshot(next)?;
        store.update_rosen_work_thread(1, 4, |thread| {
            thread.rosen_change = Some("original".into());
        })?;
        Ok(store)
    }

    fn assert_items(store: &StateStore, status: WorkStatus, change: &str, thread: Option<u32>) {
        let items = &store.state.projects[0].rosen_work_items;
        assert_eq!(items.len(), 3);
        for key in [
            WorkKey::Task { identifier: 1 },
            WorkKey::Task { identifier: 2 },
            WorkKey::issue("owner/repo", 7),
        ] {
            assert!(items.iter().any(|item| {
                item.key == key
                    && item.status == status
                    && item.change.as_deref() == Some(change)
                    && item.thread_identifier == thread
                    && item.claimed_at.is_some()
            }));
        }
    }

    fn event(
        store: &mut StateStore,
        sequence: u64,
        change: &str,
        progress: RosenProgress,
        commit: Option<&str>,
    ) -> Result<(), StoreError> {
        let evidence = WorkEvidence {
            commit,
            revert_reason: Some("the build failed"),
        };
        store.save_rosen_work_progress(1, sequence, change, (progress, ""), evidence)?;
        Ok(())
    }
}
