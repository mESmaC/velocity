//! Dispatch rule for Velocity's own project (#601): a `blocked_by` issue whose land
//! touched agent tooling counts as satisfied only once the running copy contains that
//! land. Until then the issue shows "waiting for restart into `<commit>`" and is not
//! dispatched.

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;

use tauri::{AppHandle, Manager};
use velocity_core::issue_dependencies::IssueReference;
use velocity_core::issue_holds::{
    BLOCKED_UPSTREAM_LABEL, TRACKING_LABEL, has_label, is_person_hold_label,
};
use velocity_core::tool_policy::agent_tooling_paths;
use velocity_core::work_pool::WorkKey;
use velocity_github::velocity_reports::VELOCITY_REPOSITORY;
use velocity_github::{GitHubIssue, IssueClient, IssueState};
use velocity_rosen::ProjectServer;
use velocity_rosen::post_land::{LandedCommitSummary, commit_contains, commits_since};
use velocity_rosen::review_swarm::landed_commit_paths;
use velocity_store::ProjectItem;
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::rosen_work::IssueVersion;
use velocity_threads::continuous_dispatch::SkippedIssue;
use velocity_updates::local_build::short_commit;

use super::Backlog;
use crate::local_build::WaitingIssueNeed;
use crate::threads::lock;

const RECENT_BASE_COMMITS: u32 = 200;

/// An open issue in Velocity's own project waiting for a restart into a build containing
/// its blocker's agent-tooling land (#601).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestartWait {
    pub issue: u32,
    pub commit: String,
    pub required_commits: Vec<String>,
    pub blockers: Vec<u32>,
    pub paths: Vec<String>,
    pub ready_after_restart: bool,
}

impl RestartWait {
    pub(crate) fn short_commit(&self) -> &str {
        short_commit(&self.commit)
    }

    pub(crate) fn label(&self) -> String {
        format!("waiting for restart into {}", self.short_commit())
    }

    pub(crate) fn hold(&self) -> velocity_github::IssueHold {
        let short = self.short_commit();
        let blockers: Vec<String> = self.blockers.iter().map(|n| format!("#{n}")).collect();
        let reason = format!(
            "Blocked by {} whose land ({short}) changed agent tooling. Dispatch starts this \
             issue once Velocity restarts into a build containing {short}.",
            blockers.join(", "),
        );
        velocity_github::IssueHold::new(self.label(), reason, self.paths.clone())
    }

    pub(crate) fn skipped(&self) -> SkippedIssue {
        SkippedIssue {
            number: self.issue,
            category: velocity_threads::continuous_dispatch::SkipCategory::Blocker,
            reason: self.label(),
        }
    }

    pub(crate) fn need(&self) -> WaitingIssueNeed {
        WaitingIssueNeed {
            issue: self.issue,
            commit: self.commit.clone(),
            required_commits: self.required_commits.clone(),
        }
    }
}

#[derive(Default)]
struct WaitStore {
    by_project: BTreeMap<u32, BTreeMap<u32, RestartWait>>,
    tooling_paths: BTreeMap<String, Vec<String>>,
    contains: BTreeMap<(String, String), bool>,
}

#[derive(Default)]
pub(crate) struct ManagedRestartWaits(Mutex<WaitStore>);

pub(crate) fn initialize(application: &AppHandle) {
    application.manage(ManagedRestartWaits::default());
}

pub(crate) fn waits_for_project(
    application: &AppHandle,
    project: u32,
) -> BTreeMap<u32, RestartWait> {
    let Some(managed) = application.try_state::<ManagedRestartWaits>() else {
        return BTreeMap::new();
    };
    lock(&managed.0, "restart waits")
        .map(|store| store.by_project.get(&project).cloned().unwrap_or_default())
        .unwrap_or_default()
}

pub(crate) fn include_restart_waits(
    application: &AppHandle,
    project: u32,
    claimed: &mut HashSet<String>,
) {
    for number in waits_for_project(application, project).keys() {
        claimed.insert(format!("issue:{number}"));
    }
}

pub(crate) fn is_waiting(application: &AppHandle, project: u32, number: u32) -> bool {
    waits_for_project(application, project).contains_key(&number)
}

/// Refreshes restart waits for Velocity's own project and pushes the waiting count to
/// `local_build` so the title bar and auto-restart react immediately (#601).
pub(super) fn refresh(
    application: &AppHandle,
    project: &ProjectItem,
    backlog: &Backlog,
    claimed: &HashSet<String>,
) {
    if !is_velocity_project(project, &backlog.repository) {
        clear_project(application, project.id);
        return;
    }
    if backlog.error.is_some() {
        return;
    }
    let Some(running) = crate::local_build::running_commit(application) else {
        clear_project(application, project.id);
        crate::local_build::update_waiting_issues(application, Vec::new());
        return;
    };
    let Ok(server) = crate::rosen::observation_server(application, project.id) else {
        return;
    };
    let recent =
        match tauri::async_runtime::block_on(commits_since(&server, &running, RECENT_BASE_COMMITS))
        {
            Ok(commits) => commits,
            Err(_) => return,
        };
    let index = RecentIndex::from_commits(&recent);
    let mut waits = BTreeMap::new();
    for issue in &backlog.issues {
        if let Some(wait) = evaluate_issue(
            application,
            &server,
            project,
            &backlog.repository,
            &running,
            &index,
            issue,
            claimed,
        ) {
            waits.insert(issue.number, wait);
        }
    }
    store_project(application, project.id, &waits);
}

/// Re-evaluates one freshly read issue right before dispatch so a newly landed blocker
/// cannot slip past a stale backlog snapshot (#601).
pub(crate) fn check_issue(
    application: &AppHandle,
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    claimed: &HashSet<String>,
) -> Option<RestartWait> {
    if !is_velocity_project(project, repository) {
        return None;
    }
    let running = crate::local_build::running_commit(application)?;
    let server = crate::rosen::observation_server(application, project.id).ok()?;
    let recent =
        tauri::async_runtime::block_on(commits_since(&server, &running, RECENT_BASE_COMMITS))
            .ok()?;
    let index = RecentIndex::from_commits(&recent);
    let wait = evaluate_issue(
        application,
        &server,
        project,
        repository,
        &running,
        &index,
        issue,
        claimed,
    );
    update_single(application, project.id, issue.number, wait.clone());
    wait
}

fn is_velocity_project(project: &ProjectItem, repository: &str) -> bool {
    project.rosen_in_use() && repository.eq_ignore_ascii_case(VELOCITY_REPOSITORY)
}

fn clear_project(application: &AppHandle, project: u32) {
    let Some(managed) = application.try_state::<ManagedRestartWaits>() else {
        return;
    };
    if let Ok(mut store) = lock(&managed.0, "restart waits") {
        store.by_project.remove(&project);
    }
}

fn store_project(application: &AppHandle, project: u32, waits: &BTreeMap<u32, RestartWait>) {
    let Some(managed) = application.try_state::<ManagedRestartWaits>() else {
        return;
    };
    let changed = match lock(&managed.0, "restart waits") {
        Ok(mut store) => {
            let previous = store.by_project.get(&project);
            let same = previous.is_some_and(|existing| existing == waits)
                || (previous.is_none() && waits.is_empty());
            if !same {
                if waits.is_empty() {
                    store.by_project.remove(&project);
                } else {
                    store.by_project.insert(project, waits.clone());
                }
            }
            !same
        }
        Err(_) => false,
    };
    let needs: Vec<WaitingIssueNeed> = waits
        .values()
        .filter(|wait| wait.ready_after_restart)
        .map(RestartWait::need)
        .collect();
    crate::local_build::update_waiting_issues(application, needs);
    if changed {
        crate::issue_panel::refresh_links(application);
    }
}

fn update_single(application: &AppHandle, project: u32, number: u32, wait: Option<RestartWait>) {
    let mut current = waits_for_project(application, project);
    match wait {
        Some(wait) => {
            current.insert(number, wait);
        }
        None => {
            current.remove(&number);
        }
    }
    store_project(application, project, &current);
}

struct RecentIndex {
    rank: BTreeMap<String, usize>,
    by_issue: BTreeMap<u32, Vec<String>>,
}

impl RecentIndex {
    fn from_commits(commits: &[LandedCommitSummary]) -> Self {
        let mut rank = BTreeMap::new();
        let mut by_issue: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        for (index, entry) in commits.iter().enumerate() {
            let commit = entry.commit.to_ascii_lowercase();
            rank.entry(commit.clone()).or_insert(index);
            for number in issue_numbers_in_summary(&entry.summary) {
                let list = by_issue.entry(number).or_default();
                if !list.iter().any(|existing| existing == &commit) {
                    list.push(commit.clone());
                }
            }
        }
        Self { rank, by_issue }
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_issue(
    application: &AppHandle,
    server: &ProjectServer,
    project: &ProjectItem,
    repository: &str,
    running: &str,
    index: &RecentIndex,
    issue: &GitHubIssue,
    claimed: &HashSet<String>,
) -> Option<RestartWait> {
    if issue.state != IssueState::Open {
        return None;
    }
    let identity = ThreadIssue::new(repository.into(), issue.number);
    let version = IssueVersion::from_github(issue);
    if issue_thread(project, &identity).is_some()
        || project.rosen_issue_completed_for_version(&identity, version)
        || project
            .landed_awaiting_integration_for_version(&identity, version)
            .is_some()
    {
        return None;
    }
    let (candidate_blockers, has_other_open) = classify_blockers(project, repository, issue);
    if candidate_blockers.is_empty() {
        return None;
    }
    let mut required_commits = Vec::new();
    let mut blockers = Vec::new();
    let mut paths = Vec::new();
    for blocker in candidate_blockers {
        let commits = blocker_commits(project, repository, blocker, index);
        for commit in commits {
            if running_contains(application, server, running, &commit, index) {
                continue;
            }
            let touched = commit_tooling_paths(application, server, &commit);
            if touched.is_empty() {
                continue;
            }
            if !blockers.contains(&blocker) {
                blockers.push(blocker);
            }
            if !required_commits.iter().any(|existing| existing == &commit) {
                required_commits.push(commit);
            }
            for path in touched {
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
    }
    if required_commits.is_empty() {
        return None;
    }
    required_commits.sort_by_key(|commit| index.rank.get(commit).copied().unwrap_or(usize::MAX));
    blockers.sort_unstable();
    paths.sort();
    let newest = required_commits.first()?.clone();
    let ready_after_restart =
        !has_other_open && eligible_for_restart_count(project, repository, issue, claimed);
    Some(RestartWait {
        issue: issue.number,
        commit: newest,
        required_commits,
        blockers,
        paths,
        ready_after_restart,
    })
}

fn classify_blockers(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
) -> (Vec<u32>, bool) {
    let mut candidates = Vec::new();
    let mut has_other_open =
        issue.open_blocker_count > u32::try_from(issue.open_blockers.len()).unwrap_or(u32::MAX);
    for blocker in &issue.open_blockers {
        let Some(reference) = IssueReference::parse(blocker) else {
            has_other_open = true;
            continue;
        };
        let repo = reference.repository_or(repository);
        let identity = ThreadIssue::new(repo.to_owned(), reference.number());
        if !project.rosen_issue_integration_green(&identity) {
            has_other_open = true;
            continue;
        }
        if repo.eq_ignore_ascii_case(VELOCITY_REPOSITORY)
            && !candidates.contains(&reference.number())
        {
            candidates.push(reference.number());
        }
    }
    for blocker in &issue.closed_blockers {
        let Some(reference) = IssueReference::parse(blocker) else {
            continue;
        };
        if reference
            .repository_or(repository)
            .eq_ignore_ascii_case(VELOCITY_REPOSITORY)
            && !candidates.contains(&reference.number())
        {
            candidates.push(reference.number());
        }
    }
    (candidates, has_other_open)
}

fn blocker_commits(
    project: &ProjectItem,
    repository: &str,
    blocker: u32,
    index: &RecentIndex,
) -> Vec<String> {
    let mut commits = Vec::new();
    let mut push = |commit: &str| {
        let lower = commit.trim().to_ascii_lowercase();
        if !lower.is_empty() && !commits.iter().any(|existing| existing == &lower) {
            commits.push(lower);
        }
    };
    if let Some(commit) = local_landed_commit(project, repository, blocker) {
        push(commit);
    }
    if let Some(logged) = index.by_issue.get(&blocker) {
        for commit in logged {
            push(commit);
        }
    }
    commits
}

fn local_landed_commit<'a>(
    project: &'a ProjectItem,
    repository: &str,
    number: u32,
) -> Option<&'a str> {
    let key = WorkKey::issue(repository, number);
    if let Some(commit) = project
        .rosen_work_item(&key)
        .and_then(|item| item.landed_commit.as_deref())
    {
        return Some(commit);
    }
    if let Some(commit) = project.threads.iter().rev().find_map(|thread| {
        let assigned = thread.issue.as_ref()?;
        (assigned.number == number && assigned.repository.eq_ignore_ascii_case(repository))
            .then(|| thread.rosen_issue.landed_commit())
            .flatten()
    }) {
        return Some(commit);
    }
    if let Some(commit) = project.retained_rosen_issues.values().find_map(|retained| {
        (retained.issue.number == number
            && retained.issue.repository.eq_ignore_ascii_case(repository))
        .then(|| retained.progress.landed_commit())
        .flatten()
    }) {
        return Some(commit);
    }
    project
        .continuous_dispatch
        .landed_holds
        .get(&number)
        .filter(|hold| hold.issue.repository.eq_ignore_ascii_case(repository))
        .map(|hold| hold.commit.as_str())
}

fn running_contains(
    application: &AppHandle,
    server: &ProjectServer,
    running: &str,
    commit: &str,
    index: &RecentIndex,
) -> bool {
    if commit.eq_ignore_ascii_case(running) || commit.starts_with(running) {
        return true;
    }
    if index.rank.contains_key(commit) {
        return false;
    }
    let cache_key = (running.to_ascii_lowercase(), commit.to_ascii_lowercase());
    if let Some(managed) = application.try_state::<ManagedRestartWaits>()
        && let Ok(store) = lock(&managed.0, "restart waits")
        && let Some(known) = store.contains.get(&cache_key)
    {
        return *known;
    }
    let contained =
        tauri::async_runtime::block_on(commit_contains(server, running, commit)).unwrap_or(true);
    if let Some(managed) = application.try_state::<ManagedRestartWaits>()
        && let Ok(mut store) = lock(&managed.0, "restart waits")
    {
        store.contains.insert(cache_key, contained);
    }
    contained
}

fn commit_tooling_paths(
    application: &AppHandle,
    server: &ProjectServer,
    commit: &str,
) -> Vec<String> {
    let key = commit.to_ascii_lowercase();
    if let Some(managed) = application.try_state::<ManagedRestartWaits>()
        && let Ok(store) = lock(&managed.0, "restart waits")
        && let Some(cached) = store.tooling_paths.get(&key)
    {
        return cached.clone();
    }
    let Ok(paths) = tauri::async_runtime::block_on(landed_commit_paths(server, &key)) else {
        return Vec::new();
    };
    let tooling = agent_tooling_paths(&paths);
    if let Some(managed) = application.try_state::<ManagedRestartWaits>()
        && let Ok(mut store) = lock(&managed.0, "restart waits")
    {
        store.tooling_paths.insert(key, tooling.clone());
    }
    tooling
}

fn eligible_for_restart_count(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    claimed: &HashSet<String>,
) -> bool {
    !issue.is_spec_question()
        && !has_label(&issue.labels, TRACKING_LABEL)
        && !has_label(&issue.labels, BLOCKED_UPSTREAM_LABEL)
        && !issue.labels.iter().any(|label| is_person_hold_label(label))
        && !issue.labels.iter().any(|label| {
            label.eq_ignore_ascii_case("release-blocker")
                || label.eq_ignore_ascii_case(IssueClient::NO_CHANGE_LABEL)
        })
        && !claimed.contains(&format!("issue:{}", issue.number))
        && !project
            .continuous_dispatch
            .attempted(repository, issue.number, &issue.updated_at)
        && !project
            .continuous_dispatch
            .reporting_no_change(repository, issue.number)
}

/// Extracts issue numbers from `(#N)` or `(#N, #M)` tokens in a landed commit summary (#601).
fn issue_numbers_in_summary(summary: &str) -> Vec<u32> {
    let mut numbers = Vec::new();
    let mut rest = summary;
    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(')') else {
            break;
        };
        let inside = &after[..close];
        for token in inside.split(|ch: char| ch.is_whitespace() || ch == ',') {
            if let Some(digits) = token.strip_prefix('#')
                && !digits.is_empty()
                && digits.bytes().all(|byte| byte.is_ascii_digit())
                && let Ok(number) = digits.parse::<u32>()
                && number > 0
                && !numbers.contains(&number)
            {
                numbers.push(number);
            }
        }
        rest = &after[close + 1..];
    }
    numbers
}

#[cfg(test)]
mod tests {
    use super::issue_numbers_in_summary;

    #[test]
    fn parses_single_and_multiple_issue_numbers_from_commit_summaries() {
        assert_eq!(
            issue_numbers_in_summary("Add project_read to velocity-graph (#600)"),
            vec![600]
        );
        assert_eq!(
            issue_numbers_in_summary("Cross-repo issue_read and graph fixes (#598, #600)"),
            vec![598, 600]
        );
        assert_eq!(
            issue_numbers_in_summary("No issue reference here"),
            Vec::<u32>::new()
        );
    }
}
