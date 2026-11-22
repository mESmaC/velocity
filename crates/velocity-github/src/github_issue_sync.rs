//! Shared GitHub -> Collapse one-way issue sync covering every GitHub issue opened or
//! changed since Collapse's import `finished_at` (#612, #614).
//!
//! Used by both Velocity's periodic sync (`src-tauri`) and pre-create synchronization
//! before Velocity creates a new Collapse issue so GitHub numbers are imported/allocated
//! before new Collapse issues take them.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use velocity_collapse::issues::{
    CollapseIssue, CollapseIssueApi, Conditional, CreateIssueRequest, UpdateIssueRequest,
};
use velocity_core::collapse::{
    ExpectedIssueIdentity, GitHubIssueSync, IssueHost, format_synced_github_issue_body,
    github_sync_marker, host_issue_matches_expected, parse_rfc3339_seconds,
    should_sync_github_issue_change,
};

use crate::{GitHubIssue, IssueClient, IssueState};

/// Summary of one GitHub -> Collapse sync pass (#614).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitHubIssueSyncOutcome {
    pub created: usize,
    pub updated: usize,
    pub used_import: bool,
    pub notes: Vec<String>,
}

impl GitHubIssueSyncOutcome {
    pub fn total_synced(&self) -> usize {
        self.created + self.updated
    }
}

/// Runs one GitHub -> Collapse sync pass asynchronously using `write_token` (#614).
///
/// Covers every GitHub issue opened or changed since Collapse's import `finished_at`.
/// When Collapse's issue cursor has not advanced past the last import (meaning no
/// post-import Collapse edits would be overwritten), runs Collapse's idempotent repository
/// import so new GitHub issues keep their exact GitHub numbers and Collapse's `next_number`
/// advances past them. Otherwise mirrors new and updated GitHub issues through the Collapse
/// issue API without clobbering unrelated Collapse issues or newer Collapse edits.
pub async fn sync_github_issues_async(
    repository_path: &Path,
    collapse_repository: &str,
    write_token: &str,
    sync: &mut GitHubIssueSync,
) -> Result<GitHubIssueSyncOutcome, String> {
    if !sync.enabled || sync.github_repository.trim().is_empty() {
        return Ok(GitHubIssueSyncOutcome::default());
    }
    sync.expires_at_secs = None;
    let Some((owner, name)) = collapse_repository.split_once('/') else {
        return Ok(GitHubIssueSyncOutcome::default());
    };
    let import_meta = refresh_import_metadata(write_token, owner, name, sync).await;
    let github_issues = read_github_issues_since(
        repository_path,
        &sync.github_repository,
        sync.import_finished_at.as_deref(),
    )?;
    let mut pending: Vec<GitHubIssue> = github_issues
        .into_iter()
        .filter(|issue| {
            should_sync_github_issue_change(
                sync,
                issue.number,
                &issue.created_at,
                &issue.updated_at,
            )
        })
        .collect();
    pending.sort_by_key(|issue| issue.number);
    if pending.is_empty() {
        return Ok(GitHubIssueSyncOutcome::default());
    }
    let api = CollapseIssueApi::new().map_err(|error| error.to_string())?;
    let outcome =
        if can_use_lossless_import(&api, write_token, owner, name, import_meta, &pending, sync)
            .await
        {
            match run_lossless_import(write_token, owner, name, &pending, sync).await {
                Ok(outcome) => outcome,
                Err(_) => {
                    mirror_issues_individually(&api, write_token, owner, name, &pending, sync)
                        .await?
                }
            }
        } else {
            mirror_issues_individually(&api, write_token, owner, name, &pending, sync).await?
        };
    if outcome.total_synced() > 0 {
        IssueClient::invalidate_open_issues_for(IssueHost::Collapse, collapse_repository);
    }
    Ok(outcome)
}

/// Runs a best-effort GitHub -> Collapse sync pass synchronously before creating a new
/// Collapse issue so GitHub issues opened since the import claim their numbers first (#614).
pub fn sync_github_issues_blocking(
    repository_path: &Path,
    collapse_repository: &str,
    sync: &mut GitHubIssueSync,
) -> Result<GitHubIssueSyncOutcome, String> {
    if !sync.enabled || sync.github_repository.trim().is_empty() {
        return Ok(GitHubIssueSyncOutcome::default());
    }
    let Some(write_token) =
        velocity_collapse::load_issues_token().map_err(|error| error.to_string())?
    else {
        return Ok(GitHubIssueSyncOutcome::default());
    };
    let path = repository_path.to_path_buf();
    let repo = collapse_repository.to_owned();
    let mut working = sync.clone();
    let result = std::thread::Builder::new()
        .name("velocity-github-issue-sync".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            let outcome = runtime.block_on(sync_github_issues_async(
                &path,
                &repo,
                &write_token,
                &mut working,
            ))?;
            Ok::<_, String>((working, outcome))
        })
        .map_err(|error| error.to_string())?
        .join()
        .map_err(|_| "GitHub issue sync worker panicked".to_owned())??;
    *sync = result.0;
    Ok(result.1)
}

#[derive(Clone, Copy, Default)]
struct ImportMetadata {
    cursor: Option<u64>,
}

async fn refresh_import_metadata(
    token: &str,
    owner: &str,
    name: &str,
    sync: &mut GitHubIssueSync,
) -> ImportMetadata {
    let Ok(status) = velocity_collapse::get_issue_import_status(token, owner, name).await else {
        return ImportMetadata::default();
    };
    if sync.import_finished_at.is_none()
        && let Some(finished_at) = status.finished_without_error_at()
    {
        sync.import_finished_at = Some(finished_at.to_owned());
    }
    if let Some(result) = &status.result {
        if sync.high_water_number == 0 && result.next_number > 1 {
            sync.high_water_number =
                u32::try_from(result.next_number.saturating_sub(1)).unwrap_or(u32::MAX);
        }
        return ImportMetadata {
            cursor: Some(result.cursor),
        };
    }
    ImportMetadata::default()
}

async fn can_use_lossless_import(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    meta: ImportMetadata,
    pending: &[GitHubIssue],
    sync: &GitHubIssueSync,
) -> bool {
    let has_unmapped = pending
        .iter()
        .any(|issue| !sync.synced_numbers.contains_key(&issue.number));
    if !has_unmapped {
        return false;
    }
    let Some(import_cursor) = meta.cursor else {
        return false;
    };
    match api.get_cursor(Some(token), owner, name, None).await {
        Ok(Some(Conditional::Modified { value, .. })) => value <= import_cursor,
        Ok(None) => import_cursor == 0,
        _ => false,
    }
}

async fn run_lossless_import(
    token: &str,
    owner: &str,
    name: &str,
    pending: &[GitHubIssue],
    sync: &mut GitHubIssueSync,
) -> Result<GitHubIssueSyncOutcome, String> {
    let status = velocity_collapse::import_github_issues_and_wait(
        token,
        owner,
        name,
        &sync.github_repository,
        None,
    )
    .await
    .map_err(|error| error.to_string())?;
    if let Some(finished_at) = status.finished_without_error_at() {
        sync.import_finished_at = Some(finished_at.to_owned());
    }
    if let Some(result) = &status.result
        && result.next_number > 1
    {
        let imported_max = u32::try_from(result.next_number.saturating_sub(1)).unwrap_or(u32::MAX);
        sync.high_water_number = sync.high_water_number.max(imported_max);
    }
    let mut created = 0;
    let mut updated = 0;
    let mut notes = Vec::new();
    for issue in pending {
        let is_new = !sync.synced_numbers.contains_key(&issue.number)
            && issue.number > sync.high_water_number.saturating_sub(pending.len() as u32);
        sync.synced_numbers.insert(issue.number, issue.number);
        sync.synced_updated_at
            .insert(issue.number, issue.updated_at.clone());
        sync.high_water_number = sync.high_water_number.max(issue.number);
        if is_new {
            created += 1;
        } else {
            updated += 1;
        }
        notes.push(format!(
            "imported GitHub {}#{} -> Collapse {owner}/{name}#{}",
            sync.github_repository, issue.number, issue.number
        ));
    }
    Ok(GitHubIssueSyncOutcome {
        created,
        updated,
        used_import: true,
        notes,
    })
}

async fn mirror_issues_individually(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    pending: &[GitHubIssue],
    sync: &mut GitHubIssueSync,
) -> Result<GitHubIssueSyncOutcome, String> {
    let existing_by_number = fetch_existing_by_number(api, token, owner, name, pending, sync).await;
    let open_collapse = fetch_open_collapse_issues(api, token, owner, name).await;
    let available_labels = fetch_collapse_labels(api, token, owner, name).await;
    let mut outcome = GitHubIssueSyncOutcome::default();
    for issue in pending {
        let existing =
            find_matching_collapse_issue(issue, sync, &existing_by_number, &open_collapse);
        if let Some(target) = existing {
            if update_if_github_newer(
                api,
                token,
                owner,
                name,
                issue,
                target,
                &available_labels,
                &sync.github_repository,
            )
            .await?
            {
                outcome.updated += 1;
                outcome.notes.push(format!(
                    "updated Collapse {owner}/{name}#{} from GitHub {}#{}",
                    target.number, sync.github_repository, issue.number
                ));
            }
            sync.synced_numbers.insert(issue.number, target.number);
            sync.synced_updated_at
                .insert(issue.number, issue.updated_at.clone());
            sync.high_water_number = sync.high_water_number.max(issue.number);
            continue;
        }
        let created =
            create_mirrored_issue(api, token, owner, name, issue, &available_labels, sync).await?;
        sync.synced_numbers.insert(issue.number, created.number);
        sync.synced_updated_at
            .insert(issue.number, issue.updated_at.clone());
        sync.high_water_number = sync.high_water_number.max(issue.number);
        outcome.created += 1;
        outcome.notes.push(format!(
            "synced GitHub {}#{} -> Collapse {owner}/{name}#{}",
            sync.github_repository, issue.number, created.number
        ));
    }
    Ok(outcome)
}

async fn create_mirrored_issue(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    issue: &GitHubIssue,
    available_labels: &[String],
    sync: &GitHubIssueSync,
) -> Result<CollapseIssue, String> {
    let body = format_synced_github_issue_body(&issue.body, &sync.github_repository, issue.number);
    let labels = match_existing_labels(&issue.labels, available_labels);
    let blocked_by = map_blockers(
        &issue.open_blockers,
        &sync.synced_numbers,
        sync.high_water_number,
    );
    let created = api
        .create_issue(
            token,
            owner,
            name,
            &CreateIssueRequest {
                title: &issue.title,
                body: &body,
                labels,
                blocked_by,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    if issue.state == IssueState::Closed {
        let _ = api
            .close_issue(token, owner, name, created.number, "completed", "")
            .await;
    }
    Ok(created)
}

#[allow(clippy::too_many_arguments)]
async fn update_if_github_newer(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    github: &GitHubIssue,
    collapse: &CollapseIssue,
    available_labels: &[String],
    github_repository: &str,
) -> Result<bool, String> {
    let gh_ts = parse_rfc3339_seconds(&github.updated_at);
    let collapse_ts = parse_rfc3339_seconds(&collapse.updated_at);
    if matches!((gh_ts, collapse_ts), (Some(gh), Some(col)) if gh <= col) {
        return Ok(false);
    }
    let want_state = match github.state {
        IssueState::Open => "open",
        IssueState::Closed => "closed",
    };
    let marker = github_sync_marker(github_repository, github.number);
    let want_body = if collapse.body.contains(&marker) {
        format_synced_github_issue_body(&github.body, github_repository, github.number)
    } else {
        github.body.clone()
    };
    let want_labels = match_existing_labels(&github.labels, available_labels);
    let current_labels: Vec<String> = collapse.labels.iter().map(|l| l.name.clone()).collect();
    if collapse.title == github.title
        && collapse.body == want_body
        && collapse.state.eq_ignore_ascii_case(want_state)
        && labels_same(&current_labels, &want_labels)
    {
        return Ok(false);
    }
    api.update_issue(
        token,
        owner,
        name,
        collapse.number,
        &UpdateIssueRequest {
            title: Some(github.title.clone()),
            body: Some(want_body),
            state: Some(want_state.to_owned()),
            labels: (!available_labels.is_empty()).then_some(want_labels),
            ..UpdateIssueRequest::default()
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(true)
}

fn find_matching_collapse_issue<'a>(
    github: &GitHubIssue,
    sync: &GitHubIssueSync,
    by_number: &'a HashMap<u32, CollapseIssue>,
    open_collapse: &'a [CollapseIssue],
) -> Option<&'a CollapseIssue> {
    if let Some(&mapped) = sync.synced_numbers.get(&github.number)
        && let Some(found) = by_number.get(&mapped)
    {
        return Some(found);
    }
    let marker = github_sync_marker(&sync.github_repository, github.number);
    if let Some(found) = open_collapse.iter().find(|row| row.body.contains(&marker)) {
        return Some(found);
    }
    if let Some(same_number) = by_number.get(&github.number) {
        let expected = ExpectedIssueIdentity {
            title: Some(&github.title),
            github_repository: Some(&sync.github_repository),
            github_number: Some(github.number),
        };
        if same_number.author.kind.eq_ignore_ascii_case("github")
            || host_issue_matches_expected(&expected, &same_number.title, &same_number.body)
        {
            return Some(same_number);
        }
    }
    None
}

async fn fetch_existing_by_number(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    pending: &[GitHubIssue],
    sync: &GitHubIssueSync,
) -> HashMap<u32, CollapseIssue> {
    let mut wanted: Vec<u32> = pending
        .iter()
        .flat_map(|issue| {
            let mapped = sync.synced_numbers.get(&issue.number).copied();
            [Some(issue.number), mapped].into_iter().flatten()
        })
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut found = HashMap::new();
    for chunk in wanted.chunks(velocity_collapse::issues::MAX_BATCH_NUMBERS) {
        if let Ok(Conditional::Modified { value, .. }) = api
            .list_issues(
                Some(token),
                owner,
                name,
                Some("all"),
                Some(chunk),
                chunk.len(),
                0,
                None,
            )
            .await
        {
            for issue in value.issues {
                found.insert(issue.number, issue);
            }
        }
    }
    found
}

async fn fetch_open_collapse_issues(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
) -> Vec<CollapseIssue> {
    let mut issues = Vec::new();
    let mut offset = 0;
    loop {
        let Ok(Conditional::Modified { value, .. }) = api
            .list_issues(
                Some(token),
                owner,
                name,
                Some("open"),
                None,
                velocity_collapse::issues::MAX_LIST_LIMIT,
                offset,
                None,
            )
            .await
        else {
            break;
        };
        let count = value.issues.len();
        issues.extend(value.issues);
        if count < velocity_collapse::issues::MAX_LIST_LIMIT {
            break;
        }
        offset += count;
    }
    issues
}

async fn fetch_collapse_labels(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
) -> Vec<String> {
    match api.list_labels(Some(token), owner, name, None).await {
        Ok(Conditional::Modified { value, .. }) => {
            value.labels.into_iter().map(|label| label.name).collect()
        }
        _ => Vec::new(),
    }
}

fn read_github_issues_since(
    repository_path: &Path,
    github_repository: &str,
    since: Option<&str>,
) -> Result<Vec<GitHubIssue>, String> {
    let client = IssueClient::for_repository(repository_path, github_repository)
        .map_err(|error| error.to_string())?;
    client
        .issues_updated_since(since)
        .map_err(|error| error.to_string())
}

fn match_existing_labels(requested: &[String], available: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter_map(|label| {
            available
                .iter()
                .find(|existing| existing.eq_ignore_ascii_case(label))
                .cloned()
        })
        .collect()
}

fn labels_same(left: &[String], right: &[String]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .all(|item| right.iter().any(|other| other.eq_ignore_ascii_case(item)))
}

fn map_blockers(
    open_blockers: &[String],
    synced_numbers: &BTreeMap<u32, u32>,
    high_water_number: u32,
) -> Vec<String> {
    let mut mapped = Vec::new();
    for blocker in open_blockers {
        if let Some(raw_number) = blocker.strip_prefix('#')
            && let Ok(number) = raw_number.parse::<u32>()
        {
            if let Some(&collapse_number) = synced_numbers.get(&number) {
                mapped.push(format!("#{collapse_number}"));
            } else if number <= high_water_number {
                mapped.push(format!("#{number}"));
            }
        }
    }
    mapped
}
