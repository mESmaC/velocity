//! One-step Move to Collapse cut-over for an existing project (#592):
//! 1. Import GitHub issues into Collapse (collapse.sh#193) and wait until caught up.
//! 2. Switch `[mirror] remote` and `collapse` root to Collapse, reusing and fast-forwarding
//!    an existing Collapse repository from Rosen's base instead of creating a new one.
//! 3. Set Issues on Collapse (#589) and CI from Rosen (#591).
//! 4. Optionally keep GitHub as a one-way downstream copy (C4), or remove it.
//! 5. Run `rosen collapse check` and report every step's result.

use std::path::Path;
use std::time::Duration;

use tauri::AppHandle;
use velocity_collapse::{
    CollapseCredentials, CollapseError, GitHubIssueImportResult, MirrorRequest,
    ProvisionDisposition, ProvisionedMirror, RepositoryActivity,
};
use velocity_core::collapse::{
    CollapseCheckItemStatus, CollapseCheckSummary, DeletedCollapseRepository,
    DeletedRepositoryAction, ForwarderState, IssueHost, MoveToCollapseStepId,
    MoveToCollapseStepResult, MoveToCollapseStepStatus, collapse_forwarding_signal,
    downstream_settings_page, github_repository_from_remote, move_to_collapse_can_switch_mirror,
    repository_name_for,
};
use velocity_core::project_health::short_commit;
use velocity_rosen::ProjectServer;
use velocity_rosen::collapse_link::{
    CollapseLink, MirrorFastForwardOutcome, RemoteBaseComparison, collapse_check,
    probe_remote_against_base, propose_collapse_link, wait_for_mirror_fast_forward,
};
use velocity_rosen::landing::LandOutcome;
use velocity_rosen::workspace_settings::SETTINGS_AGENT;

use crate::collapse_mirror::{
    DownstreamResult, MirrorSubject, NOT_CONNECTED, ProjectCollapseSettings, configure_downstream,
    deleted_repository_view, finish_downstream_setup, load_credentials, mirror_subject,
    read_project_collapse_settings, refresh_health, refusal_reason, relay_note, saved_relay_allow,
    toast, typed_repository,
};

/// How long Move to Collapse waits for Rosen's background push to finish fast-forwarding
/// an existing Collapse repository from Rosen's base.
const MIRROR_SYNC_WAIT: Duration = Duration::from_secs(120);

/// Input for `move_to_collapse` (§3.1, #592, #610).
#[derive(Clone, serde::Deserialize, specta::Type)]
pub struct MoveToCollapseRequest {
    pub project_identifier: u32,
    /// Optional explicit Collapse `owner/name` (or `name`). When omitted, Velocity links a
    /// matching existing repository on Collapse before creating a new one.
    #[serde(default)]
    pub collapse_repository: Option<String>,
    /// Optional explicit GitHub `owner/name` to import issues from and optionally forward to.
    #[serde(default)]
    pub github_repository: Option<String>,
    /// Keep GitHub as a one-way downstream copy (C4) when `true`; clear it when `false`.
    pub keep_github_downstream: bool,
    /// Optional transient GitHub token used only for the issue import request; never stored.
    #[serde(default)]
    pub github_import_token: Option<String>,
    /// Optional owner choice when the Collapse name is held by a deleted copy pending purge (#610).
    #[serde(default)]
    pub deleted_action: Option<DeletedRepositoryAction>,
}

/// Step-by-step outcome of `move_to_collapse` (§3.1, #592, #610).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, specta::Type)]
pub struct MoveToCollapseResult {
    pub ok: bool,
    pub collapse_repository: Option<String>,
    pub github_repository: Option<String>,
    pub steps: Vec<MoveToCollapseStepResult>,
    pub collapse_check: Option<CollapseCheckSummary>,
    pub settings: ProjectCollapseSettings,
    /// Populated when the target Collapse name is held by a deleted repository pending purge (#610).
    #[serde(default)]
    pub deleted_repository: Option<DeletedCollapseRepository>,
}

/// `move_to_collapse` (§3.1): runs the full GitHub-to-Collapse cut-over for a Rosen project,
/// reports each step's result and `rosen collapse check`, and is safe to re-run (#592).
#[tauri::command]
#[specta::specta]
pub async fn move_to_collapse(
    application: AppHandle,
    request: MoveToCollapseRequest,
) -> Result<MoveToCollapseResult, String> {
    let project = request.project_identifier;
    let subject = mirror_subject(&application, project)?;
    let credentials = load_credentials().await?.ok_or(NOT_CONNECTED)?;
    let server = crate::rosen::server(&application, project).await?;
    let dashboard = velocity_collapse::repository_activity(&credentials)
        .await
        .unwrap_or_default();
    let github_repository =
        resolve_github_repository(&application, project, &subject, &server, &request).await?;
    let (target_owner, target_name) = resolve_collapse_target(
        &credentials,
        &subject,
        github_repository.as_deref(),
        request.collapse_repository.as_deref(),
        &dashboard,
    )?;
    let mirror_request = MirrorRequest {
        owner: Some(&target_owner),
        name: &target_name,
        default_branch: &subject.branch,
        description: "",
    };
    let provisioned = match provision_for_cutover(
        &credentials,
        &target_owner,
        &target_name,
        &mirror_request,
        request.deleted_action,
    )
    .await
    {
        Ok(provisioned) => provisioned,
        Err(CollapseError::DeletedPending(deleted)) => {
            let settings = read_project_collapse_settings(&application, project).await?;
            return Ok(deleted_repository_result(
                deleted_repository_view(*deleted),
                github_repository,
                settings,
            ));
        }
        Err(error) => return Err(error.to_string()),
    };
    let collapse_repository = provisioned.repository();
    let github_repository = match github_repository {
        Some(repo) => Some(repo),
        None => {
            resolve_github_from_collapse(&credentials, &provisioned.owner, &provisioned.name).await
        }
    };
    let (steps, check) = run_cutover_steps(
        &application,
        &subject,
        &credentials,
        &server,
        &provisioned,
        github_repository.as_deref(),
        &request,
        &dashboard,
    )
    .await?;
    let _ = refresh_health(&application).await;
    let settings = read_project_collapse_settings(&application, project).await?;
    let import_passed = steps.iter().any(|step| {
        step.step == MoveToCollapseStepId::ImportIssues
            && move_to_collapse_can_switch_mirror(step.status)
    });
    let ok = import_passed
        && steps
            .iter()
            .all(|step| step.status != MoveToCollapseStepStatus::Failed)
        && check.as_ref().is_none_or(|report| report.ok);
    let summary = if ok {
        format!(
            "{}: Move to Collapse finished · {collapse_repository}",
            subject.name
        )
    } else {
        format!(
            "{}: Move to Collapse needs attention · {collapse_repository}",
            subject.name
        )
    };
    let _ = toast(&application, summary);
    Ok(MoveToCollapseResult {
        ok,
        collapse_repository: Some(collapse_repository),
        github_repository,
        steps,
        collapse_check: check,
        settings,
        deleted_repository: None,
    })
}

/// `import_collapse_issues` (§3.1): runs the same GitHub issue import step Move to Collapse
/// uses, for a Collapse-hosted project whose issues were never imported (#612).
#[tauri::command]
#[specta::specta]
pub async fn import_collapse_issues(
    application: AppHandle,
    project_identifier: u32,
) -> Result<MoveToCollapseStepResult, String> {
    let subject = mirror_subject(&application, project_identifier)?;
    let collapse_repository = subject
        .repository
        .clone()
        .ok_or("This project does not mirror to Collapse")?;
    let (owner, name) = collapse_repository
        .split_once('/')
        .ok_or("Invalid Collapse repository name")?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    let mut github_repository = resolve_github_repository_for_project(
        &application,
        project_identifier,
        &subject,
        &server,
        None,
    )
    .await?;
    if github_repository.is_none()
        && let Some(credentials) = load_credentials().await?
    {
        github_repository = resolve_github_from_collapse(&credentials, owner, name).await;
    }
    let (step, high_water, import_finished_at) = run_import_issues(
        &subject.repository_path,
        owner,
        name,
        github_repository.as_deref(),
        None,
    )
    .await;
    if step.status == MoveToCollapseStepStatus::Passed {
        velocity_github::IssueClient::invalidate_open_issues_for(
            IssueHost::Collapse,
            &collapse_repository,
        );
        crate::project_health::mark_collapse_issues_imported(
            &application,
            project_identifier,
            &collapse_repository,
        );
        if let Some(github) = github_repository.as_deref() {
            let _ = crate::collapse_issue_sync::enable_post_switch_sync(
                &application,
                project_identifier,
                github,
                high_water,
                import_finished_at.as_deref(),
                true,
            )
            .await;
        }
        let _ = toast(&application, format!("{}: {}", subject.name, step.summary));
    } else {
        crate::project_health::record_collapse_import_error(
            &application,
            project_identifier,
            &collapse_repository,
            &step.summary,
        );
    }
    Ok(step)
}

async fn provision_for_cutover(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
    request: &MirrorRequest<'_>,
    deleted_action: Option<DeletedRepositoryAction>,
) -> Result<ProvisionedMirror, CollapseError> {
    match deleted_action {
        None => velocity_collapse::ensure_mirror(credentials, request).await,
        Some(DeletedRepositoryAction::Restore) => {
            velocity_collapse::restore_mirror(credentials, owner, name).await
        }
        Some(DeletedRepositoryAction::PurgeAndCreate) => {
            velocity_collapse::purge_and_create_mirror(credentials, request).await
        }
    }
}

fn deleted_repository_result(
    deleted: DeletedCollapseRepository,
    github_repository: Option<String>,
    settings: ProjectCollapseSettings,
) -> MoveToCollapseResult {
    let summary = deleted.notice();
    let detail = if deleted.purge_available {
        "Restore and link the deleted copy (fast-forwarding it from Rosen's base), purge it now and create a fresh repository, or manage it in repository settings."
    } else {
        "Restore and link the deleted copy (fast-forwarding it from Rosen's base) or manage it in repository settings."
    };
    let waiting = "Waiting for the deleted Collapse repository to be restored or purged.";
    let steps = vec![
        skipped_step(
            MoveToCollapseStepId::ImportIssues,
            "Import GitHub issues",
            waiting,
        ),
        failed_step(
            MoveToCollapseStepId::SwitchMirror,
            "Switch mirror to Collapse",
            summary,
            Some(detail.into()),
            Some(deleted.settings_url.clone()),
            Some("Open repository settings".into()),
        ),
        skipped_step(
            MoveToCollapseStepId::SetIssuesAndCi,
            "Set Issues on Collapse & CI from Rosen",
            waiting,
        ),
        skipped_step(
            MoveToCollapseStepId::ConfigureDownstream,
            "GitHub downstream mirror",
            waiting,
        ),
        skipped_step(
            MoveToCollapseStepId::CollapseCheck,
            "Run rosen collapse check",
            waiting,
        ),
    ];
    MoveToCollapseResult {
        ok: false,
        collapse_repository: Some(deleted.repository.clone()),
        github_repository,
        steps,
        collapse_check: None,
        settings,
        deleted_repository: Some(deleted),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_cutover_steps(
    application: &AppHandle,
    subject: &MirrorSubject,
    credentials: &CollapseCredentials,
    server: &ProjectServer,
    provisioned: &ProvisionedMirror,
    github_repository: Option<&str>,
    request: &MoveToCollapseRequest,
    dashboard: &[RepositoryActivity],
) -> Result<(Vec<MoveToCollapseStepResult>, Option<CollapseCheckSummary>), String> {
    let mut steps = Vec::with_capacity(5);
    let (import_step, high_water, import_finished_at) = run_import_issues(
        &subject.repository_path,
        &provisioned.owner,
        &provisioned.name,
        github_repository,
        request.github_import_token.as_deref(),
    )
    .await;
    let can_switch = move_to_collapse_can_switch_mirror(import_step.status);
    steps.push(import_step);
    if !can_switch {
        let reason =
            "Waiting for GitHub issue import to pass; fix Step 1 and re-run Move to Collapse.";
        steps.push(skipped_step(
            MoveToCollapseStepId::SwitchMirror,
            "Switch mirror to Collapse",
            reason,
        ));
        steps.push(skipped_step(
            MoveToCollapseStepId::SetIssuesAndCi,
            "Set Issues on Collapse & CI from Rosen",
            reason,
        ));
        steps.push(skipped_step(
            MoveToCollapseStepId::ConfigureDownstream,
            "GitHub downstream mirror",
            reason,
        ));
        steps.push(skipped_step(
            MoveToCollapseStepId::CollapseCheck,
            "Run rosen collapse check",
            reason,
        ));
        return Ok((steps, None));
    }
    let mirror_step = step_switch_mirror(
        application,
        request.project_identifier,
        subject,
        server,
        provisioned,
    )
    .await?;
    let mirror_failed = mirror_step.status == MoveToCollapseStepStatus::Failed;
    steps.push(mirror_step);
    if mirror_failed {
        let reason = "Waiting for the Collapse mirror link and fast-forward to succeed.";
        steps.push(skipped_step(
            MoveToCollapseStepId::SetIssuesAndCi,
            "Set Issues on Collapse & CI from Rosen",
            reason,
        ));
        steps.push(skipped_step(
            MoveToCollapseStepId::ConfigureDownstream,
            "GitHub downstream mirror",
            reason,
        ));
    } else {
        steps.push(
            step_set_issues_and_ci(
                application,
                request.project_identifier,
                provisioned,
                github_repository,
                high_water,
                import_finished_at.as_deref(),
            )
            .await?,
        );
        steps.push(
            step_configure_downstream(
                application,
                request.project_identifier,
                credentials,
                provisioned,
                github_repository,
                request.keep_github_downstream,
                dashboard,
            )
            .await?,
        );
    }
    let (check_step, check_summary) = step_collapse_check(server).await;
    steps.push(check_step);
    Ok((steps, check_summary))
}

async fn resolve_github_repository(
    application: &AppHandle,
    project: u32,
    subject: &MirrorSubject,
    server: &ProjectServer,
    request: &MoveToCollapseRequest,
) -> Result<Option<String>, String> {
    resolve_github_repository_for_project(
        application,
        project,
        subject,
        server,
        request.github_repository.as_deref(),
    )
    .await
}

async fn resolve_github_repository_for_project(
    application: &AppHandle,
    project: u32,
    subject: &MirrorSubject,
    server: &ProjectServer,
    explicit_override: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(explicit) = explicit_override
        && !explicit.trim().is_empty()
    {
        return velocity_new_project::parse_github_repository(explicit)
            .map_err(|error| error.to_string())
            .map(|parsed| parsed.map(|name| name.full_name()));
    }
    if let Some(saved) = subject.github_remote.as_deref() {
        return Ok(Some(
            github_repository_from_remote(saved).unwrap_or_else(|| saved.to_owned()),
        ));
    }
    if let Some(from_mirror) = velocity_rosen::mirror_remote(server)
        .await
        .ok()
        .flatten()
        .and_then(|remote| github_repository_from_remote(&remote))
    {
        return Ok(Some(from_mirror));
    }
    let from_store = crate::state::with_store(application, move |store| {
        let item = store.project(project)?;
        item.collapse_mirror_health
            .as_ref()
            .and_then(|health| health.downstream_github.clone())
            .or_else(|| {
                item.github_issue_sync
                    .as_ref()
                    .map(|sync| sync.github_repository.trim().to_owned())
                    .filter(|repo| !repo.is_empty())
            })
    })
    .await
    .ok()
    .flatten();
    if from_store.is_some() {
        return Ok(from_store);
    }
    let path = subject.repository_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        velocity_github::checkout_repository(&path).map(|name| name.full_name())
    })
    .await
    .map_err(|error| error.to_string())
}

async fn resolve_github_from_collapse(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Option<String> {
    if let Ok(Some(remote)) =
        velocity_collapse::repository_downstream(credentials, owner, name).await
        && let Some(github) = github_repository_from_remote(&remote)
    {
        return Some(github);
    }
    let token = tauri::async_runtime::spawn_blocking(velocity_collapse::resolve_issues_token)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()?;
    let status = velocity_collapse::get_issue_import_status(&token, owner, name)
        .await
        .ok()?;
    for candidate in [status.github_repository, status.suggested_github_repository] {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}

fn resolve_collapse_target(
    credentials: &CollapseCredentials,
    subject: &MirrorSubject,
    github_repository: Option<&str>,
    explicit: Option<&str>,
    dashboard: &[RepositoryActivity],
) -> Result<(String, String), String> {
    if let Some(raw) = explicit
        && !raw.trim().is_empty()
    {
        let cleaned = typed_repository(raw);
        let (owner, name) = match cleaned.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() => {
                (owner.to_owned(), name.to_owned())
            }
            None if !cleaned.is_empty() => (credentials.account.clone(), cleaned),
            _ => return Err("Enter a Collapse repository as owner/name or name".into()),
        };
        return Ok(match_dashboard_casing(dashboard, &owner, &name));
    }
    if let Some(existing) = subject.repository.as_deref()
        && let Some((owner, name)) = existing.split_once('/')
    {
        return Ok(match_dashboard_casing(dashboard, owner, name));
    }
    if let Some(github) = github_repository
        && let Some(row) = dashboard.iter().find(|row| row.is(github))
    {
        return Ok((row.owner.clone(), row.name.clone()));
    }
    let slug = github_repository
        .and_then(|repo| repo.split_once('/').map(|(_, name)| name.to_owned()))
        .or_else(|| repository_name_for(&subject.name))
        .ok_or("The project's name has no letters or digits to name a Collapse repository")?;
    let default_candidate = format!("{}/{slug}", credentials.account);
    if let Some(row) = dashboard.iter().find(|row| row.is(&default_candidate)) {
        return Ok((row.owner.clone(), row.name.clone()));
    }
    Ok((credentials.account.clone(), slug))
}

fn match_dashboard_casing(
    dashboard: &[RepositoryActivity],
    owner: &str,
    name: &str,
) -> (String, String) {
    let full = format!("{owner}/{name}");
    if let Some(row) = dashboard.iter().find(|row| row.is(&full)) {
        return (row.owner.clone(), row.name.clone());
    }
    (owner.to_owned(), name.to_owned())
}

async fn run_import_issues(
    repository_path: &Path,
    owner: &str,
    name: &str,
    github_repository: Option<&str>,
    github_import_token: Option<&str>,
) -> (MoveToCollapseStepResult, Option<u32>, Option<String>) {
    let title = "Import GitHub issues".to_owned();
    let Some(github) = github_repository else {
        return (
            skipped_step(
                MoveToCollapseStepId::ImportIssues,
                &title,
                "No GitHub repository configured for issue import. Enter the GitHub repository (owner/repo) above and re-run Move to Collapse.",
            ),
            None,
            None,
        );
    };
    let token = tauri::async_runtime::spawn_blocking(velocity_collapse::resolve_issues_token)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    let Some(token) = token else {
        if verify_github_repository_has_no_issues(repository_path, github).await {
            return (empty_github_passed_step(title, github), Some(0), None);
        }
        return (
            failed_step(
                MoveToCollapseStepId::ImportIssues,
                &title,
                "No Collapse token available for issue import.".into(),
                Some("Save a Collapse personal access token with the 'issues' scope in Collapse settings, then re-run Move to Collapse.".into()),
                None,
                None,
            ),
            None,
            None,
        );
    };
    match velocity_collapse::import_github_issues_and_wait(
        &token,
        owner,
        name,
        github,
        github_import_token,
    )
    .await
    {
        Ok(status) if status.has_finished() => {
            let finished_at = status.finished_without_error_at().map(str::to_owned);
            let high_water = status.result.as_ref().map(|counts| {
                u32::try_from(counts.next_number.saturating_sub(1)).unwrap_or(u32::MAX)
            });
            let (summary, detail) = format_import_summary(github, status.result.as_ref());
            (
                MoveToCollapseStepResult {
                    step: MoveToCollapseStepId::ImportIssues,
                    title,
                    status: MoveToCollapseStepStatus::Passed,
                    summary,
                    detail,
                    action_url: None,
                    action_label: None,
                },
                high_water,
                finished_at,
            )
        }
        Ok(_) => {
            if verify_github_repository_has_no_issues(repository_path, github).await {
                return (empty_github_passed_step(title, github), Some(0), None);
            }
            (
                failed_step(
                    MoveToCollapseStepId::ImportIssues,
                    &title,
                    format!("Collapse did not record a completed import for {github}."),
                    Some("Re-run Move to Collapse once Collapse finishes importing issues.".into()),
                    None,
                    None,
                ),
                None,
                None,
            )
        }
        Err(failure) => {
            if verify_github_repository_has_no_issues(repository_path, github).await {
                return (empty_github_passed_step(title, github), Some(0), None);
            }
            let detail = if failure.needs_issues_token {
                Some("Issue import requires a Collapse personal access token with the 'issues' scope. Save one in Collapse settings below and re-run Move to Collapse.".into())
            } else {
                failure
                    .moved_to
                    .map(|moved| format!("GitHub reports this repository moved to {moved}."))
            };
            let action_label = failure
                .action_url
                .as_ref()
                .map(|_| "Open GitHub App permissions".to_owned());
            (
                failed_step(
                    MoveToCollapseStepId::ImportIssues,
                    &title,
                    failure.message,
                    detail,
                    failure.action_url,
                    action_label,
                ),
                None,
                None,
            )
        }
    }
}

fn empty_github_passed_step(title: String, github: &str) -> MoveToCollapseStepResult {
    MoveToCollapseStepResult {
        step: MoveToCollapseStepId::ImportIssues,
        title,
        status: MoveToCollapseStepStatus::Passed,
        summary: format!("Verified {github} has no GitHub issues; nothing to import."),
        detail: None,
        action_url: None,
        action_label: None,
    }
}

async fn verify_github_repository_has_no_issues(repository_path: &Path, github: &str) -> bool {
    let directory = repository_path.to_path_buf();
    let repo = github.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        velocity_github::IssueClient::for_repository(&directory, &repo)
            .and_then(|client| client.total_issue_count())
            .is_ok_and(|count| count == 0)
    })
    .await
    .unwrap_or(false)
}

fn format_import_summary(
    github: &str,
    result: Option<&GitHubIssueImportResult>,
) -> (String, Option<String>) {
    let Some(counts) = result else {
        return (
            format!("GitHub issues from {github} are caught up on Collapse."),
            None,
        );
    };
    let summary = format!(
        "Imported from {github}: {} created, {} updated, {} unchanged.",
        counts.issues_created, counts.issues_updated, counts.issues_unchanged
    );
    let detail = format!(
        "Comments: +{} / ~{} / ={} · Labels: +{} / ~{} · Dependencies: +{} / -{} · Next issue #{}",
        counts.comments_created,
        counts.comments_updated,
        counts.comments_unchanged,
        counts.labels_created,
        counts.labels_updated,
        counts.dependencies_added,
        counts.dependencies_removed,
        counts.next_number
    );
    (summary, Some(detail))
}

async fn step_switch_mirror(
    application: &AppHandle,
    project: u32,
    subject: &MirrorSubject,
    server: &ProjectServer,
    provisioned: &ProvisionedMirror,
) -> Result<MoveToCollapseStepResult, String> {
    let title = "Switch mirror to Collapse".to_owned();
    let repo = provisioned.repository();
    let current_remote = velocity_rosen::mirror_remote(server).await.ok().flatten();
    if current_remote
        .as_deref()
        .is_none_or(|remote| !remote.eq_ignore_ascii_case(&provisioned.mirror_url))
    {
        crate::rosen_mirror::check_and_catch_up(application, project).await;
    }
    let comparison = probe_remote_against_base(server, &provisioned.mirror_url)
        .await
        .map_err(|error| error.to_string())?;
    if let RemoteBaseComparison::NotAncestor {
        branch,
        base_commit,
        remote_tip,
    } = &comparison
    {
        return Ok(failed_step(
            MoveToCollapseStepId::SwitchMirror,
            &title,
            format!(
                "Existing Collapse repository {repo} ({branch} at {}) has commits not in Rosen's base ({}) and cannot be fast-forwarded.",
                short_commit(remote_tip),
                short_commit(base_commit)
            ),
            Some("No changes were landed to rosen.toml. Reconcile the Collapse branch or choose another Collapse repository name.".into()),
            None,
            None,
        ));
    }
    let root = provisioned.collapse_root();
    let link = CollapseLink {
        remote: Some(&provisioned.mirror_url),
        root: &root,
    };
    let landed_note =
        match propose_and_land_link(application, project, server, &subject.name, &link).await? {
            Ok(note) => note,
            Err(refusal) => {
                return Ok(failed_step(
                    MoveToCollapseStepId::SwitchMirror,
                    &title,
                    refusal,
                    None,
                    None,
                    None,
                ));
            }
        };
    let sync = wait_for_mirror_fast_forward(server, &provisioned.mirror_url, MIRROR_SYNC_WAIT)
        .await
        .map_err(|error| error.to_string())?;
    crate::rosen_mirror::refresh_for_owner(application, project).await?;
    Ok(format_mirror_step(
        title,
        &repo,
        provisioned.disposition,
        &comparison,
        sync,
        landed_note,
    ))
}

async fn propose_and_land_link(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    project_name: &str,
    link: &CollapseLink<'_>,
) -> Result<Result<String, String>, String> {
    let change = propose_collapse_link(server, link)
        .await
        .map_err(|error| error.to_string())?;
    let mut note = match change {
        None => "rosen.toml already points to Collapse".to_owned(),
        Some(change) => {
            let outcome = velocity_rosen::landing::land(server, SETTINGS_AGENT, &change)
                .await
                .map_err(|error| error.to_string())?;
            match outcome {
                LandOutcome::Landed { .. } => {
                    "Updated rosen.toml mirror and collapse root".to_owned()
                }
                LandOutcome::Refused(refusal) => {
                    return Ok(Err(format!(
                        "Could not land rosen.toml change ({change}): {}",
                        refusal_reason(refusal)
                    )));
                }
            }
        }
    };
    let allow = saved_relay_allow(application, project).await;
    note.push_str(&relay_note(server, project_name, &allow).await);
    Ok(Ok(note))
}

fn format_mirror_step(
    title: String,
    repo: &str,
    disposition: ProvisionDisposition,
    comparison: &RemoteBaseComparison,
    sync: MirrorFastForwardOutcome,
    landed_note: String,
) -> MoveToCollapseStepResult {
    let linked_prefix = match disposition {
        ProvisionDisposition::Created => format!("Created and linked {repo}"),
        ProvisionDisposition::PurgedAndCreated => {
            format!("Purged deleted {repo}, created a fresh copy, and linked it")
        }
        ProvisionDisposition::Restored => format!("Restored and linked {repo}"),
        ProvisionDisposition::Reused => format!("Linked {repo}"),
    };
    match sync {
        MirrorFastForwardOutcome::Synced { base_commit } => {
            let tip = base_commit.as_deref().map(short_commit).unwrap_or("base");
            let summary = format_synced_summary(repo, disposition, comparison, tip);
            MoveToCollapseStepResult {
                step: MoveToCollapseStepId::SwitchMirror,
                title,
                status: MoveToCollapseStepStatus::Passed,
                summary,
                detail: Some(landed_note),
                action_url: None,
                action_label: None,
            }
        }
        MirrorFastForwardOutcome::NotOwner => failed_step(
            MoveToCollapseStepId::SwitchMirror,
            &title,
            format!(
                "{linked_prefix} in rosen.toml, but this Rosen server is not marked as mirror owner."
            ),
            Some(landed_note),
            None,
            None,
        ),
        MirrorFastForwardOutcome::Rejected { error } => failed_step(
            MoveToCollapseStepId::SwitchMirror,
            &title,
            format!("{linked_prefix}, but Rosen's mirror push was rejected: {error}"),
            Some(landed_note),
            None,
            None,
        ),
        MirrorFastForwardOutcome::StillPushing { error } => MoveToCollapseStepResult {
            step: MoveToCollapseStepId::SwitchMirror,
            title,
            status: MoveToCollapseStepStatus::Warning,
            summary: format!("{linked_prefix}; Rosen is still pushing the base in the background."),
            detail: Some(match error {
                Some(message) => format!("{landed_note} · {message}"),
                None => landed_note,
            }),
            action_url: None,
            action_label: None,
        },
    }
}

fn format_synced_summary(
    repo: &str,
    disposition: ProvisionDisposition,
    comparison: &RemoteBaseComparison,
    tip: &str,
) -> String {
    match (disposition, comparison) {
        (ProvisionDisposition::Created, _) => {
            format!("Created {repo} and pushed Rosen's base ({tip}).")
        }
        (ProvisionDisposition::PurgedAndCreated, _) => {
            format!("Purged deleted {repo}, created a fresh copy, and pushed Rosen's base ({tip}).")
        }
        (ProvisionDisposition::Restored, RemoteBaseComparison::BehindBy { commits_behind, .. }) => {
            format!(
                "Restored {repo} and fast-forwarded {commits_behind} {} to {tip}.",
                if *commits_behind == 1 {
                    "commit"
                } else {
                    "commits"
                }
            )
        }
        (ProvisionDisposition::Restored, RemoteBaseComparison::EmptyRemote { .. }) => {
            format!("Restored empty {repo} and pushed Rosen's base ({tip}).")
        }
        (ProvisionDisposition::Restored, RemoteBaseComparison::UpToDate { .. }) => {
            format!("Restored {repo} (already up to date at {tip}).")
        }
        (ProvisionDisposition::Restored, _) => {
            format!("Restored {repo} and synced Rosen's base ({tip}).")
        }
        (ProvisionDisposition::Reused, RemoteBaseComparison::BehindBy { commits_behind, .. }) => {
            format!(
                "Linked existing {repo} and fast-forwarded {commits_behind} {} to {tip}.",
                if *commits_behind == 1 {
                    "commit"
                } else {
                    "commits"
                }
            )
        }
        (ProvisionDisposition::Reused, RemoteBaseComparison::EmptyRemote { .. }) => {
            format!("Linked existing empty {repo} and pushed Rosen's base ({tip}).")
        }
        (ProvisionDisposition::Reused, RemoteBaseComparison::UpToDate { .. }) => {
            format!("Linked existing {repo} (already up to date at {tip}).")
        }
        (ProvisionDisposition::Reused, _) => {
            format!("Linked existing {repo} and synced Rosen's base ({tip}).")
        }
    }
}

async fn step_set_issues_and_ci(
    application: &AppHandle,
    project: u32,
    provisioned: &ProvisionedMirror,
    github_repository: Option<&str>,
    high_water_number: Option<u32>,
    import_finished_at: Option<&str>,
) -> Result<MoveToCollapseStepResult, String> {
    let repo = provisioned.repository();
    let refresh = crate::state::try_with_store(application, move |store| {
        let changed = store
            .save_issue_host(project, IssueHost::Collapse)
            .map_err(|error| error.to_string())?;
        Ok(changed.then(|| store.layout_refresh()))
    })
    .await?;
    if let Some(refresh) = refresh {
        let handle = application.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::layout::apply_refresh_and_publish(&handle, refresh)
        })
        .await
        .map_err(|error| error.to_string())??;
    }
    velocity_github::IssueClient::invalidate_open_issues_for(IssueHost::Collapse, &repo);
    crate::project_health::mark_collapse_issues_imported(application, project, &repo);
    crate::project_health::sync_host_source_change(application, project);
    crate::project_health::sync_host_issue_problems(application, project);
    if let Some(github) = github_repository {
        let _ = crate::collapse_issue_sync::enable_post_switch_sync(
            application,
            project,
            github,
            high_water_number,
            import_finished_at,
            false,
        )
        .await;
    }
    crate::base_ci::wake(application);
    let has_issues_token =
        tauri::async_runtime::spawn_blocking(velocity_collapse::load_issues_token)
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
            .is_some();
    let detail = if has_issues_token {
        Some("Dispatch, AIRC issue tools, base CI repairs, release gate and pin tracking now run against Collapse and Rosen. GitHub issues opened or changed since the import sync one-way into Collapse until turned off in Collapse settings.".into())
    } else {
        Some("Issues and CI switched to Collapse and Rosen. Save a Collapse PAT with 'issues' scope in Collapse settings below to allow commenting, closing and syncing GitHub issues.".into())
    };
    Ok(MoveToCollapseStepResult {
        step: MoveToCollapseStepId::SetIssuesAndCi,
        title: "Set Issues on Collapse & CI from Rosen".into(),
        status: if has_issues_token {
            MoveToCollapseStepStatus::Passed
        } else {
            MoveToCollapseStepStatus::Warning
        },
        summary: format!("Issues on Collapse ({repo}) and CI from Rosen are active."),
        detail,
        action_url: None,
        action_label: None,
    })
}

async fn step_configure_downstream(
    application: &AppHandle,
    project: u32,
    credentials: &CollapseCredentials,
    provisioned: &ProvisionedMirror,
    github_repository: Option<&str>,
    keep_github_downstream: bool,
    dashboard: &[RepositoryActivity],
) -> Result<MoveToCollapseStepResult, String> {
    let title = "GitHub downstream mirror".to_owned();
    let collapse_repo = provisioned.repository();
    if keep_github_downstream {
        return keep_downstream(
            application,
            project,
            credentials,
            provisioned,
            github_repository,
            dashboard,
            title,
        )
        .await;
    }
    remove_downstream(
        application,
        project,
        credentials,
        provisioned,
        &collapse_repo,
        dashboard,
        title,
    )
    .await
}

async fn keep_downstream(
    application: &AppHandle,
    project: u32,
    credentials: &CollapseCredentials,
    provisioned: &ProvisionedMirror,
    github_repository: Option<&str>,
    dashboard: &[RepositoryActivity],
    title: String,
) -> Result<MoveToCollapseStepResult, String> {
    let Some(github) = github_repository else {
        return Ok(skipped_step(
            MoveToCollapseStepId::ConfigureDownstream,
            &title,
            "No GitHub repository configured for one-way downstream forwarding.",
        ));
    };
    save_project_github_remote(application, project, Some(github.to_owned())).await?;
    let collapse_repo = provisioned.repository();
    if let Some(existing) =
        existing_matching_downstream(credentials, provisioned, github, dashboard).await
    {
        crate::project_health::clear_collapse_forwarding(application, project);
        return Ok(MoveToCollapseStepResult {
            step: MoveToCollapseStepId::ConfigureDownstream,
            title,
            status: MoveToCollapseStepStatus::Passed,
            summary: format!(
                "Keeping one-way GitHub downstream copy: Collapse already forwards to {github} ({existing})."
            ),
            detail: None,
            action_url: None,
            action_label: None,
        });
    }
    let result =
        configure_downstream(credentials, &provisioned.owner, &provisioned.name, github).await;
    let step = match &result {
        DownstreamResult::Configured { github_repository } => MoveToCollapseStepResult {
            step: MoveToCollapseStepId::ConfigureDownstream,
            title,
            status: MoveToCollapseStepStatus::Passed,
            summary: format!("Keeping one-way GitHub downstream copy ({github_repository})."),
            detail: None,
            action_url: None,
            action_label: None,
        },
        DownstreamResult::NeedsFinish {
            github_repository,
            page_url,
            reason,
            ..
        } => failed_step(
            MoveToCollapseStepId::ConfigureDownstream,
            &title,
            collapse_forwarding_signal(reason),
            Some(format!(
                "Set the GitHub downstream ({github_repository}) by hand in Collapse repository settings, then re-run Move to Collapse."
            )),
            downstream_settings_page(&collapse_repo).or_else(|| page_url.clone()),
            Some("Open Collapse settings".into()),
        ),
    };
    let _ = finish_downstream_setup(application, project, &collapse_repo, Some(result));
    Ok(step)
}

async fn existing_matching_downstream(
    credentials: &CollapseCredentials,
    provisioned: &ProvisionedMirror,
    github: &str,
    dashboard: &[RepositoryActivity],
) -> Option<String> {
    let collapse_repo = provisioned.repository();
    if let Some(row) = dashboard.iter().find(|row| row.is(&collapse_repo))
        && !matches!(
            ForwarderState::from_collapse(&row.forward.state),
            ForwarderState::Unconfigured | ForwarderState::Stopped
        )
        && row
            .downstream_remote()
            .as_deref()
            .and_then(github_repository_from_remote)
            .is_some_and(|existing| existing.eq_ignore_ascii_case(github))
    {
        return Some(row.forward.state.clone());
    }
    let settings = velocity_collapse::downstream::get_downstream(
        credentials,
        &provisioned.owner,
        &provisioned.name,
    )
    .await
    .ok()?;
    if settings
        .github_repository()
        .is_some_and(|existing| existing.eq_ignore_ascii_case(github))
        && !settings.state.status.eq_ignore_ascii_case("stopped")
    {
        let label = if settings.state.status.trim().is_empty() {
            "configured".to_owned()
        } else {
            settings.state.status
        };
        return Some(label);
    }
    None
}

async fn remove_downstream(
    application: &AppHandle,
    project: u32,
    credentials: &CollapseCredentials,
    provisioned: &ProvisionedMirror,
    collapse_repo: &str,
    dashboard: &[RepositoryActivity],
    title: String,
) -> Result<MoveToCollapseStepResult, String> {
    save_project_github_remote(application, project, None).await?;
    crate::project_health::clear_collapse_forwarding(application, project);
    crate::base_ci::wake(application);
    let existing_remote = dashboard
        .iter()
        .find(|row| row.is(collapse_repo))
        .and_then(RepositoryActivity::downstream_remote);
    let existing_remote = match existing_remote {
        Some(remote) => Some(remote),
        None => velocity_collapse::repository_downstream(
            credentials,
            &provisioned.owner,
            &provisioned.name,
        )
        .await
        .ok()
        .flatten(),
    };
    if let Some(remote) = existing_remote {
        let shown = github_repository_from_remote(&remote).unwrap_or(remote);
        return Ok(MoveToCollapseStepResult {
            step: MoveToCollapseStepId::ConfigureDownstream,
            title,
            status: MoveToCollapseStepStatus::Warning,
            summary: format!(
                "Removed GitHub from Velocity; remove Collapse's downstream forwarder ({shown}) in browser settings."
            ),
            detail: Some(
                "Collapse requires a signed-in browser session to delete a downstream remote."
                    .into(),
            ),
            action_url: downstream_settings_page(collapse_repo),
            action_label: Some("Open Collapse downstream settings".into()),
        });
    }
    Ok(MoveToCollapseStepResult {
        step: MoveToCollapseStepId::ConfigureDownstream,
        title,
        status: MoveToCollapseStepStatus::Passed,
        summary: "Removed GitHub downstream; Velocity and Collapse no longer reference GitHub."
            .into(),
        detail: None,
        action_url: None,
        action_label: None,
    })
}

async fn save_project_github_remote(
    application: &AppHandle,
    project: u32,
    repository: Option<String>,
) -> Result<(), String> {
    let refresh = crate::state::try_with_store(application, move |store| {
        let changed = store
            .save_github_remote(project, repository)
            .map_err(|error| error.to_string())?;
        if changed {
            let _ = store.acknowledge_host_sources(project);
        }
        Ok(changed.then(|| store.layout_refresh()))
    })
    .await?;
    if let Some(refresh) = refresh {
        let handle = application.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::layout::apply_refresh_and_publish(&handle, refresh)
        })
        .await
        .map_err(|error| error.to_string())??;
    }
    Ok(())
}

async fn step_collapse_check(
    server: &ProjectServer,
) -> (MoveToCollapseStepResult, Option<CollapseCheckSummary>) {
    let title = "Run rosen collapse check".to_owned();
    match collapse_check(server).await {
        Ok(report) => {
            let passed = report
                .checks
                .iter()
                .filter(|item| item.status == CollapseCheckItemStatus::Pass)
                .count();
            let moved = report
                .checks
                .iter()
                .filter(|item| item.status == CollapseCheckItemStatus::Moved)
                .count();
            let failed = report
                .checks
                .iter()
                .filter(|item| item.status == CollapseCheckItemStatus::Fail)
                .count();
            let skipped = report
                .checks
                .iter()
                .filter(|item| item.status == CollapseCheckItemStatus::Skipped)
                .count();
            let (status, summary) = if failed > 0 {
                (
                    MoveToCollapseStepStatus::Failed,
                    format!(
                        "{failed} of {} Collapse check(s) failed ({passed} passed).",
                        report.checks.len()
                    ),
                )
            } else if moved > 0 {
                (
                    MoveToCollapseStepStatus::Warning,
                    format!("{passed} passed, {moved} moved, {skipped} skipped."),
                )
            } else {
                (
                    MoveToCollapseStepStatus::Passed,
                    format!("All {passed} Collapse checks passed ({skipped} skipped)."),
                )
            };
            let step = MoveToCollapseStepResult {
                step: MoveToCollapseStepId::CollapseCheck,
                title,
                status,
                summary,
                detail: None,
                action_url: None,
                action_label: None,
            };
            (step, Some(report))
        }
        Err(error) => (
            failed_step(
                MoveToCollapseStepId::CollapseCheck,
                &title,
                format!("rosen collapse check failed: {error}"),
                None,
                None,
                None,
            ),
            None,
        ),
    }
}

fn skipped_step(
    step: MoveToCollapseStepId,
    title: &str,
    summary: &str,
) -> MoveToCollapseStepResult {
    MoveToCollapseStepResult {
        step,
        title: title.to_owned(),
        status: MoveToCollapseStepStatus::Skipped,
        summary: summary.to_owned(),
        detail: None,
        action_url: None,
        action_label: None,
    }
}

fn failed_step(
    step: MoveToCollapseStepId,
    title: &str,
    summary: String,
    detail: Option<String>,
    action_url: Option<String>,
    action_label: Option<String>,
) -> MoveToCollapseStepResult {
    MoveToCollapseStepResult {
        step,
        title: title.to_owned(),
        status: MoveToCollapseStepStatus::Failed,
        summary,
        detail,
        action_url,
        action_label,
    }
}
