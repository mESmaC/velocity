//! Shared GitHub -> Collapse one-way issue sync covering every GitHub issue opened or
//! changed since Collapse's import `finished_at` (#612, #614, #628).
//!
//! Used by both Velocity's periodic sync (`src-tauri`) and pre-create synchronization
//! before Velocity creates a new Collapse issue so GitHub numbers are imported/allocated
//! before new Collapse issues take them. Rewrites mapped same-repository references
//! (`#N`, `owner/name#N`, and GitHub issue URLs) in mirrored bodies and comments to the
//! mapped Collapse numbers with the original kept in brackets (for example `#626 (GitHub #615)`),
//! and retries references whose issues were not synced yet on later passes (#628).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use velocity_collapse::issues::{
    CollapseComment, CollapseIssue, CollapseIssueApi, Conditional, CreateIssueRequest,
    UpdateIssueRequest,
};
use velocity_core::collapse::{
    ExpectedIssueIdentity, GitHubIssueSync, IssueHost, format_synced_github_comment_body,
    format_synced_github_issue_body, github_comment_sync_marker, github_sync_marker,
    host_issue_matches_expected, parse_rfc3339_seconds, ready_reference_rewrite_numbers,
    rewrite_github_references_for_collapse, should_sync_github_issue_change,
};

use crate::issues::SyncedGitHubComment;
use crate::{GitHubIssue, IssueClient, IssueState};

const COMMENT_SYNC_MARKER_PREFIX: &str = "<!-- velocity-github-comment-sync:";

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

/// Runs one GitHub -> Collapse sync pass asynchronously using `write_token` (#614, #628).
///
/// Covers every GitHub issue opened or changed since Collapse's import `finished_at`.
/// When Collapse's issue cursor has not advanced past the last import (meaning no
/// post-import Collapse edits would be overwritten), runs Collapse's idempotent repository
/// import so new GitHub issues keep their exact GitHub numbers and Collapse's `next_number`
/// advances past them. Otherwise mirrors new and updated GitHub issues and comments through
/// the Collapse issue API without clobbering unrelated Collapse issues or newer Collapse edits.
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
    let pending = collect_pending_github_issues(repository_path, sync)?;
    if pending.is_empty() && ready_reference_rewrite_numbers(sync).is_empty() {
        return Ok(GitHubIssueSyncOutcome::default());
    }
    let api = CollapseIssueApi::new().map_err(|error| error.to_string())?;
    let mut outcome = run_sync_pass(
        repository_path,
        &api,
        write_token,
        owner,
        name,
        import_meta,
        &pending,
        sync,
    )
    .await?;
    rewrite_pending_references(&api, write_token, owner, name, sync, &mut outcome).await?;
    if outcome.total_synced() > 0 {
        IssueClient::invalidate_open_issues_for(IssueHost::Collapse, collapse_repository);
    }
    Ok(outcome)
}

fn collect_pending_github_issues(
    repository_path: &Path,
    sync: &GitHubIssueSync,
) -> Result<Vec<GitHubIssue>, String> {
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
    Ok(pending)
}

#[allow(clippy::too_many_arguments)]
async fn run_sync_pass(
    repository_path: &Path,
    api: &CollapseIssueApi,
    write_token: &str,
    owner: &str,
    name: &str,
    import_meta: ImportMetadata,
    pending: &[GitHubIssue],
    sync: &mut GitHubIssueSync,
) -> Result<GitHubIssueSyncOutcome, String> {
    let github_repo = sync.github_repository.clone();
    let mut read_comments =
        |issue_number: u32| read_github_issue_comments(repository_path, &github_repo, issue_number);
    if can_use_lossless_import(api, write_token, owner, name, import_meta, pending, sync).await
        && let Ok(outcome) = run_lossless_import(write_token, owner, name, pending, sync).await
    {
        return Ok(outcome);
    }
    mirror_issues_individually(
        api,
        write_token,
        owner,
        name,
        pending,
        sync,
        &mut read_comments,
    )
    .await
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

async fn mirror_issues_individually<F>(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    pending: &[GitHubIssue],
    sync: &mut GitHubIssueSync,
    read_comments: &mut F,
) -> Result<GitHubIssueSyncOutcome, String>
where
    F: FnMut(u32) -> Result<Vec<SyncedGitHubComment>, String>,
{
    if pending.is_empty() {
        return Ok(GitHubIssueSyncOutcome::default());
    }
    let existing_by_number = fetch_existing_by_number(api, token, owner, name, pending, sync).await;
    let open_collapse = fetch_open_collapse_issues(api, token, owner, name).await;
    let available_labels = fetch_collapse_labels(api, token, owner, name).await;
    let mut outcome = GitHubIssueSyncOutcome::default();
    for issue in pending {
        let existing =
            find_matching_collapse_issue(issue, sync, &existing_by_number, &open_collapse);
        mirror_single_issue(
            api,
            token,
            owner,
            name,
            issue,
            existing,
            &available_labels,
            sync,
            read_comments,
            &mut outcome,
        )
        .await?;
    }
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
async fn mirror_single_issue<F>(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    issue: &GitHubIssue,
    existing: Option<&CollapseIssue>,
    available_labels: &[String],
    sync: &mut GitHubIssueSync,
    read_comments: &mut F,
    outcome: &mut GitHubIssueSyncOutcome,
) -> Result<(), String>
where
    F: FnMut(u32) -> Result<Vec<SyncedGitHubComment>, String>,
{
    let (collapse_number, mut unresolved) = if let Some(target) = existing {
        let (body_updated, body_unresolved) = update_if_github_newer(
            api,
            token,
            owner,
            name,
            issue,
            target,
            available_labels,
            sync,
        )
        .await?;
        if body_updated {
            outcome.updated += 1;
            outcome.notes.push(format!(
                "updated Collapse {owner}/{name}#{} from GitHub {}#{}",
                target.number, sync.github_repository, issue.number
            ));
        }
        (target.number, body_unresolved)
    } else {
        let (created, body_unresolved) =
            create_mirrored_issue(api, token, owner, name, issue, available_labels, sync).await?;
        outcome.created += 1;
        outcome.notes.push(format!(
            "synced GitHub {}#{} -> Collapse {owner}/{name}#{}",
            sync.github_repository, issue.number, created.number
        ));
        (created.number, body_unresolved)
    };
    // Record the mapping before syncing comments so a transient comment error never
    // duplicates the mirrored Collapse issue on retry.
    sync.synced_numbers.insert(issue.number, collapse_number);
    sync.high_water_number = sync.high_water_number.max(issue.number);
    let comments = read_comments(issue.number)?;
    let (comments_changed, comment_unresolved) = sync_issue_comments(
        api,
        token,
        owner,
        name,
        issue.number,
        collapse_number,
        &comments,
        sync,
    )
    .await?;
    if comments_changed > 0 {
        outcome.updated += 1;
        outcome.notes.push(format!(
            "synced {comments_changed} comment(s) on Collapse {owner}/{name}#{collapse_number} from GitHub {}#{}",
            sync.github_repository, issue.number
        ));
    }
    unresolved.extend(comment_unresolved);
    update_pending_rewrites(sync, issue.number, unresolved);
    sync.synced_updated_at
        .insert(issue.number, issue.updated_at.clone());
    Ok(())
}

async fn create_mirrored_issue(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    issue: &GitHubIssue,
    available_labels: &[String],
    sync: &GitHubIssueSync,
) -> Result<(CollapseIssue, BTreeSet<u32>), String> {
    let collapse_repo = format!("{owner}/{name}");
    let rewritten = rewrite_github_references_for_collapse(
        &issue.body,
        &sync.github_repository,
        &collapse_repo,
        &sync.synced_numbers,
    );
    let body =
        format_synced_github_issue_body(&rewritten.text, &sync.github_repository, issue.number);
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
    Ok((created, rewritten.unresolved))
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
    sync: &GitHubIssueSync,
) -> Result<(bool, BTreeSet<u32>), String> {
    let collapse_repo = format!("{owner}/{name}");
    let rewritten = rewrite_github_references_for_collapse(
        &github.body,
        &sync.github_repository,
        &collapse_repo,
        &sync.synced_numbers,
    );
    let gh_ts = parse_rfc3339_seconds(&github.updated_at);
    let collapse_ts = parse_rfc3339_seconds(&collapse.updated_at);
    if matches!((gh_ts, collapse_ts), (Some(gh), Some(col)) if gh <= col) {
        return Ok((false, rewritten.unresolved));
    }
    let want_state = match github.state {
        IssueState::Open => "open",
        IssueState::Closed => "closed",
    };
    let marker = github_sync_marker(&sync.github_repository, github.number);
    let want_body = if collapse.body.contains(&marker) {
        format_synced_github_issue_body(&rewritten.text, &sync.github_repository, github.number)
    } else {
        rewritten.text
    };
    let want_labels = match_existing_labels(&github.labels, available_labels);
    let current_labels: Vec<String> = collapse.labels.iter().map(|l| l.name.clone()).collect();
    if collapse.title == github.title
        && collapse.body == want_body
        && collapse.state.eq_ignore_ascii_case(want_state)
        && labels_same(&current_labels, &want_labels)
    {
        return Ok((false, rewritten.unresolved));
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
    Ok((true, rewritten.unresolved))
}

#[allow(clippy::too_many_arguments)]
async fn sync_issue_comments(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    github_number: u32,
    collapse_number: u32,
    github_comments: &[SyncedGitHubComment],
    sync: &GitHubIssueSync,
) -> Result<(usize, BTreeSet<u32>), String> {
    if github_comments.is_empty() {
        return Ok((0, BTreeSet::new()));
    }
    let existing = fetch_collapse_comments(api, token, owner, name, collapse_number).await?;
    let collapse_repo = format!("{owner}/{name}");
    let mut changed = 0;
    let mut unresolved = BTreeSet::new();
    for comment in github_comments {
        let comment_id = comment.id.trim();
        if comment_id.is_empty() {
            continue;
        }
        let rewritten = rewrite_github_references_for_collapse(
            &comment.body,
            &sync.github_repository,
            &collapse_repo,
            &sync.synced_numbers,
        );
        unresolved.extend(rewritten.unresolved);
        let url = (!comment.url.trim().is_empty()).then_some(comment.url.as_str());
        let want_body = format_synced_github_comment_body(
            &rewritten.text,
            comment.author.as_deref(),
            &sync.github_repository,
            github_number,
            comment_id,
            url,
        );
        let marker = github_comment_sync_marker(&sync.github_repository, github_number, comment_id);
        if let Some(found) = existing.iter().find(|row| row.body.contains(&marker)) {
            if should_update_synced_comment(found, comment, &want_body) {
                api.update_comment(token, owner, name, collapse_number, found.id, &want_body)
                    .await
                    .map_err(|error| error.to_string())?;
                changed += 1;
            }
        } else {
            api.create_comment(token, owner, name, collapse_number, &want_body)
                .await
                .map_err(|error| error.to_string())?;
            changed += 1;
        }
    }
    Ok((changed, unresolved))
}

fn should_update_synced_comment(
    collapse: &CollapseComment,
    github: &SyncedGitHubComment,
    want_body: &str,
) -> bool {
    if collapse.body == want_body {
        return false;
    }
    let gh_ts = parse_rfc3339_seconds(&github.updated_at)
        .or_else(|| parse_rfc3339_seconds(&github.created_at));
    let col_ts = parse_rfc3339_seconds(&collapse.updated_at);
    !matches!((gh_ts, col_ts), (Some(gh), Some(col)) if gh < col)
}

/// Rewrites previously mirrored Collapse issue bodies and synced comments whose unmapped
/// GitHub references have since landed in `sync.synced_numbers` (#628).
async fn rewrite_pending_references(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    sync: &mut GitHubIssueSync,
    outcome: &mut GitHubIssueSyncOutcome,
) -> Result<(), String> {
    for github_number in ready_reference_rewrite_numbers(sync) {
        let Some(&collapse_number) = sync.synced_numbers.get(&github_number) else {
            sync.pending_reference_rewrites.remove(&github_number);
            continue;
        };
        let remaining =
            rewrite_single_pending_issue(api, token, owner, name, collapse_number, sync, outcome)
                .await?;
        if let Some(unresolved) = remaining {
            update_pending_rewrites(sync, github_number, unresolved);
        }
    }
    Ok(())
}

async fn rewrite_single_pending_issue(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    collapse_number: u32,
    sync: &GitHubIssueSync,
    outcome: &mut GitHubIssueSyncOutcome,
) -> Result<Option<BTreeSet<u32>>, String> {
    let Conditional::Modified { value: issue, .. } = api
        .get_issue(Some(token), owner, name, collapse_number, None)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let collapse_repo = format!("{owner}/{name}");
    let body_rewrite = rewrite_github_references_for_collapse(
        &issue.body,
        &sync.github_repository,
        &collapse_repo,
        &sync.synced_numbers,
    );
    let mut remaining = body_rewrite.unresolved;
    let mut updated = false;
    if body_rewrite.changed {
        api.update_issue(
            token,
            owner,
            name,
            collapse_number,
            &UpdateIssueRequest {
                body: Some(body_rewrite.text),
                ..UpdateIssueRequest::default()
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        updated = true;
    }
    let comments = if !issue.comments.is_empty() || issue.comment_count == 0 {
        issue.comments
    } else {
        fetch_collapse_comments(api, token, owner, name, collapse_number).await?
    };
    for comment in comments {
        if !comment.body.contains(COMMENT_SYNC_MARKER_PREFIX) {
            continue;
        }
        let rewritten = rewrite_github_references_for_collapse(
            &comment.body,
            &sync.github_repository,
            &collapse_repo,
            &sync.synced_numbers,
        );
        remaining.extend(rewritten.unresolved);
        if rewritten.changed {
            api.update_comment(
                token,
                owner,
                name,
                collapse_number,
                comment.id,
                &rewritten.text,
            )
            .await
            .map_err(|error| error.to_string())?;
            updated = true;
        }
    }
    if updated {
        outcome.updated += 1;
        outcome.notes.push(format!(
            "rewrote newly mapped GitHub issue references on Collapse {owner}/{name}#{collapse_number}"
        ));
    }
    Ok(Some(remaining))
}

fn update_pending_rewrites(
    sync: &mut GitHubIssueSync,
    github_number: u32,
    unresolved: BTreeSet<u32>,
) {
    if unresolved.is_empty() {
        sync.pending_reference_rewrites.remove(&github_number);
    } else {
        sync.pending_reference_rewrites
            .insert(github_number, unresolved.into_iter().collect());
    }
}

async fn fetch_collapse_comments(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
    number: u32,
) -> Result<Vec<CollapseComment>, String> {
    match api
        .list_comments(Some(token), owner, name, number, None)
        .await
        .map_err(|error| error.to_string())?
    {
        Conditional::Modified { value, .. } => Ok(value.comments),
        Conditional::NotModified => Ok(Vec::new()),
    }
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

fn read_github_issue_comments(
    repository_path: &Path,
    github_repository: &str,
    number: u32,
) -> Result<Vec<SyncedGitHubComment>, String> {
    let client = IssueClient::for_repository(repository_path, github_repository)
        .map_err(|error| error.to_string())?;
    client
        .read_github_comments_for_sync(number)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use velocity_collapse::issues::{CollapseAuthor, CollapseComment, CollapseIssue};
    use velocity_core::collapse::GitHubIssueSync;
    use velocity_core::issue_dependencies::{explicit_text_blockers, is_owner_comment};

    use super::{
        CollapseIssueApi, GitHubIssue, IssueHost, IssueState, SyncedGitHubComment,
        mirror_issues_individually, rewrite_pending_references,
    };

    struct MockCollapseState {
        next_number: u32,
        next_comment_id: u64,
        issues: BTreeMap<u32, CollapseIssue>,
    }

    impl MockCollapseState {
        fn new(next_number: u32) -> Self {
            Self {
                next_number,
                next_comment_id: 1,
                issues: BTreeMap::new(),
            }
        }
    }

    fn spawn_mock_collapse_server(
        state: Arc<Mutex<MockCollapseState>>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut socket) = stream else {
                    break;
                };
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    let Some((method, path, body)) = read_http_request(&mut socket) else {
                        return;
                    };
                    let (status, payload) = handle_mock_request(&state, &method, &path, &body);
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = socket.write_all(response.as_bytes());
                });
            }
        });
        Ok(format!("http://{address}"))
    }

    fn read_http_request(socket: &mut TcpStream) -> Option<(String, String, String)> {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        let mut header_end = None;
        let mut content_length = 0usize;
        loop {
            let read = socket.read(&mut chunk).ok()?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if header_end.is_none()
                && let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n")
            {
                header_end = Some(pos + 4);
                let headers = String::from_utf8_lossy(&buffer[..pos]);
                for line in headers.lines() {
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        content_length = value.trim().parse().unwrap_or(0);
                    }
                }
            }
            if let Some(end) = header_end
                && buffer.len() >= end + content_length
            {
                break;
            }
        }
        let end = header_end?;
        let header_text = String::from_utf8_lossy(&buffer[..end]);
        let first_line = header_text.lines().next()?;
        let mut parts = first_line.split_whitespace();
        let method = parts.next()?.to_owned();
        let path = parts.next()?.to_owned();
        let body = String::from_utf8_lossy(&buffer[end..buffer.len().min(end + content_length)])
            .into_owned();
        Some((method, path, body))
    }

    fn handle_mock_request(
        state: &Arc<Mutex<MockCollapseState>>,
        method: &str,
        raw_path: &str,
        body: &str,
    ) -> (&'static str, String) {
        let path = raw_path.split('?').next().unwrap_or(raw_path);
        let prefix = "/api/repositories/mESmaC/velocity/issues";
        let Some(tail) = path.strip_prefix(prefix) else {
            return (
                "404 Not Found",
                json!({"error": "unknown path"}).to_string(),
            );
        };
        let mut guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if tail == "/labels" && method == "GET" {
            return ("200 OK", json!({"cursor": 1, "labels": []}).to_string());
        }
        if tail.is_empty() && method == "GET" {
            let issues: Vec<CollapseIssue> = guard.issues.values().cloned().collect();
            return ("200 OK", json!({"cursor": 1, "issues": issues}).to_string());
        }
        if tail.is_empty() && method == "POST" {
            let payload: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let number = guard.next_number;
            guard.next_number += 1;
            let issue = CollapseIssue {
                number,
                title: payload["title"].as_str().unwrap_or("").to_owned(),
                body: payload["body"].as_str().unwrap_or("").to_owned(),
                state: "open".to_owned(),
                reason: String::new(),
                author: CollapseAuthor::default(),
                labels: Vec::new(),
                blocked_by: Vec::new(),
                blocks: Vec::new(),
                comment_count: 0,
                comments: Vec::new(),
                created_at: "2026-11-22T10:00:00Z".to_owned(),
                updated_at: "2026-11-22T10:00:00Z".to_owned(),
                closed_at: None,
            };
            guard.issues.insert(number, issue.clone());
            return ("201 Created", json!(issue).to_string());
        }
        let segments: Vec<&str> = tail.trim_start_matches('/').split('/').collect();
        let Ok(number) = segments.first().copied().unwrap_or("").parse::<u32>() else {
            return ("404 Not Found", json!({"error": "bad issue"}).to_string());
        };
        match (method, segments.as_slice()) {
            ("GET", [_]) => match guard.issues.get(&number) {
                Some(issue) => ("200 OK", json!(issue).to_string()),
                None => ("404 Not Found", json!({"error": "missing"}).to_string()),
            },
            ("PATCH", [_]) => {
                let payload: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let Some(issue) = guard.issues.get_mut(&number) else {
                    return ("404 Not Found", json!({"error": "missing"}).to_string());
                };
                if let Some(title) = payload.get("title").and_then(Value::as_str) {
                    issue.title = title.to_owned();
                }
                if let Some(new_body) = payload.get("body").and_then(Value::as_str) {
                    issue.body = new_body.to_owned();
                }
                if let Some(new_state) = payload.get("state").and_then(Value::as_str) {
                    issue.state = new_state.to_owned();
                }
                ("200 OK", json!(issue).to_string())
            }
            ("GET", [_, "comments"]) => {
                let comments = guard
                    .issues
                    .get(&number)
                    .map(|issue| issue.comments.clone())
                    .unwrap_or_default();
                (
                    "200 OK",
                    json!({"cursor": 1, "comments": comments}).to_string(),
                )
            }
            ("POST", [_, "comments"]) => {
                let payload: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let id = guard.next_comment_id;
                guard.next_comment_id += 1;
                let Some(issue) = guard.issues.get_mut(&number) else {
                    return ("404 Not Found", json!({"error": "missing"}).to_string());
                };
                let comment = CollapseComment {
                    id,
                    issue_number: number,
                    author: CollapseAuthor::default(),
                    body: payload["body"].as_str().unwrap_or("").to_owned(),
                    created_at: "2026-11-22T10:00:00Z".to_owned(),
                    updated_at: "2026-11-22T10:00:00Z".to_owned(),
                };
                issue.comments.push(comment.clone());
                issue.comment_count = issue.comments.len();
                ("201 Created", json!(comment).to_string())
            }
            ("PATCH", [_, "comments", raw_id]) => {
                let Ok(comment_id) = raw_id.parse::<u64>() else {
                    return (
                        "400 Bad Request",
                        json!({"error": "bad comment id"}).to_string(),
                    );
                };
                let payload: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let Some(issue) = guard.issues.get_mut(&number) else {
                    return ("404 Not Found", json!({"error": "missing"}).to_string());
                };
                let Some(comment) = issue.comments.iter_mut().find(|row| row.id == comment_id)
                else {
                    return (
                        "404 Not Found",
                        json!({"error": "missing comment"}).to_string(),
                    );
                };
                if let Some(new_body) = payload.get("body").and_then(Value::as_str) {
                    comment.body = new_body.to_owned();
                }
                ("200 OK", json!(comment).to_string())
            }
            _ => (
                "404 Not Found",
                json!({"error": "unhandled route"}).to_string(),
            ),
        }
    }

    fn sample_github_issue(number: u32, title: &str, body: &str) -> GitHubIssue {
        GitHubIssue {
            id: format!("I_{number}"),
            number,
            title: title.to_owned(),
            body: body.to_owned(),
            url: format!("https://github.com/mESmaC/velocity/issues/{number}"),
            state: IssueState::Open,
            author: None,
            created_at: "2026-11-22T10:00:00Z".to_owned(),
            updated_at: "2026-11-22T10:00:00Z".to_owned(),
            closed_at: None,
            reopened_at: None,
            reopened_by_import: false,
            reopened_by_non_person: false,
            labels: Vec::new(),
            scout: None,
            milestone: None,
            open_blocker_count: 0,
            open_blockers: Vec::new(),
            closed_blockers: Vec::new(),
            host: IssueHost::GitHub,
        }
    }

    /// Risk (#628): Syncing GitHub issues and comments onto Collapse with shifted numbers must
    /// rewrite mapped references in bodies and comments to `#<collapse> (GitHub #<gh>)` so
    /// dispatch blocks on the mapped Collapse issue, while leaving references to not-yet-synced
    /// issues intact and rewriting them automatically on the next pass once those issues sync.
    #[test]
    fn sync_rewrites_body_comment_and_deferred_references_across_passes()
    -> Result<(), Box<dyn std::error::Error>> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let state = Arc::new(Mutex::new(MockCollapseState::new(626)));
            let base_url = spawn_mock_collapse_server(Arc::clone(&state))?;
            let api = CollapseIssueApi::with_base_url(&base_url)?;
            let mut sync = GitHubIssueSync {
                enabled: true,
                github_repository: "mESmaC/velocity".into(),
                high_water_number: 614,
                ..GitHubIssueSync::default()
            };

            // Pass 1: GitHub #615 -> Collapse #626 and GitHub #616 -> Collapse #627.
            // #616 references #615 (mapped in this pass) and #617 (not synced yet) in both its
            // body and a comment.
            let gh_615 = sample_github_issue(615, "Linux build", "Build Velocity on Linux.");
            let gh_616 = sample_github_issue(
                616,
                "Linux packaging",
                "Blocked by #615, which makes Velocity build and run on Linux.\nAlso waits for #617.",
            );
            let comments_pass1 = BTreeMap::from([(
                616_u32,
                vec![SyncedGitHubComment {
                    id: "IC_100".into(),
                    author: Some("owner".into()),
                    body: "Waits for mESmaC/velocity#615 and https://github.com/mESmaC/velocity/issues/617."
                        .into(),
                    created_at: "2026-11-22T10:05:00Z".into(),
                    updated_at: "2026-11-22T10:05:00Z".into(),
                    url: "https://github.com/mESmaC/velocity/issues/616#issuecomment-100".into(),
                }],
            )]);
            let mut outcome1 = mirror_issues_individually(
                &api,
                "test-token",
                "mESmaC",
                "velocity",
                &[gh_615, gh_616],
                &mut sync,
                &mut |num| Ok(comments_pass1.get(&num).cloned().unwrap_or_default()),
            )
            .await?;
            rewrite_pending_references(
                &api,
                "test-token",
                "mESmaC",
                "velocity",
                &mut sync,
                &mut outcome1,
            )
            .await?;

            assert_eq!(sync.synced_numbers.get(&615), Some(&626));
            assert_eq!(sync.synced_numbers.get(&616), Some(&627));
            assert_eq!(sync.pending_reference_rewrites.get(&616), Some(&vec![617]));

            // Persisted GitHubIssueSync records with pending_reference_rewrites survive save/load,
            // and legacy records without the field deserialize cleanly.
            let legacy: GitHubIssueSync = serde_json::from_str(
                r#"{"enabled":true,"github_repository":"mESmaC/velocity","high_water_number":614}"#,
            )?;
            assert!(legacy.pending_reference_rewrites.is_empty());
            let persisted: GitHubIssueSync =
                serde_json::from_str(&serde_json::to_string(&sync)?)?;
            assert_eq!(persisted, sync);

            let (body_after_pass1, comment_after_pass1) = {
                let guard = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let issue_627 = guard.issues.get(&627).ok_or("missing Collapse #627")?;
                let comment = issue_627
                    .comments
                    .first()
                    .ok_or("missing synced comment on Collapse #627")?;
                (issue_627.body.clone(), comment.body.clone())
            };
            assert!(body_after_pass1.starts_with(
                "Blocked by #626 (GitHub #615), which makes Velocity build and run on Linux.\nAlso waits for #617."
            ));
            assert!(comment_after_pass1.starts_with(
                "Waits for mESmaC/velocity#626 (GitHub #615) and https://github.com/mESmaC/velocity/issues/617."
            ));
            assert!(is_owner_comment(&comment_after_pass1));

            // Dispatch text blocker scanner blocks on #626 (and #617 while unmapped), never #615.
            let body_blockers: Vec<String> = explicit_text_blockers(&body_after_pass1)
                .into_iter()
                .map(|reference| reference.to_string())
                .collect();
            assert_eq!(body_blockers, vec!["#626", "#617"]);

            // Pass 2: GitHub #617 arrives and maps to Collapse #628. GitHub #616 itself was NOT
            // edited, yet Collapse #627's body and synced comment both rewrite #617 -> #628.
            let gh_617 = sample_github_issue(617, "Linux updater", "Ship Linux updater.");
            let mut outcome2 = mirror_issues_individually(
                &api,
                "test-token",
                "mESmaC",
                "velocity",
                &[gh_617],
                &mut sync,
                &mut |_| Ok(Vec::new()),
            )
            .await?;
            rewrite_pending_references(
                &api,
                "test-token",
                "mESmaC",
                "velocity",
                &mut sync,
                &mut outcome2,
            )
            .await?;

            assert_eq!(sync.synced_numbers.get(&617), Some(&628));
            assert!(sync.pending_reference_rewrites.is_empty());

            let (body_after_pass2, comment_after_pass2) = {
                let guard = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let issue_627 = guard.issues.get(&627).ok_or("missing Collapse #627")?;
                let comment = issue_627
                    .comments
                    .first()
                    .ok_or("missing synced comment on Collapse #627")?;
                (issue_627.body.clone(), comment.body.clone())
            };
            assert!(body_after_pass2.starts_with(
                "Blocked by #626 (GitHub #615), which makes Velocity build and run on Linux.\nAlso waits for #628 (GitHub #617)."
            ));
            assert!(comment_after_pass2.starts_with(
                "Waits for mESmaC/velocity#626 (GitHub #615) and https://collapse.sh/mESmaC/velocity/issues/628 (GitHub #617)."
            ));
            assert!(
                body_after_pass2
                    .contains("_Synced from GitHub: https://github.com/mESmaC/velocity/issues/616_")
            );
            let pass2_body_blockers: Vec<String> = explicit_text_blockers(&body_after_pass2)
                .into_iter()
                .map(|reference| reference.to_string())
                .collect();
            assert_eq!(pass2_body_blockers, vec!["#626", "#628"]);
            let pass2_comment_blockers: Vec<String> =
                explicit_text_blockers(&comment_after_pass2)
                    .into_iter()
                    .map(|reference| reference.to_string())
                    .collect();
            assert_eq!(
                pass2_comment_blockers,
                vec!["mESmaC/velocity#626", "mESmaC/velocity#628"]
            );
            Ok(())
        })
    }
}
