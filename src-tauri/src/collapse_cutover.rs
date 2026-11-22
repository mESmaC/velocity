//! One-step Move to Collapse cut-over for an existing project (#592):
//! 1. Import GitHub issues into Collapse (collapse.sh#193) and wait until caught up.
//! 2. Switch `[mirror] remote` and `collapse` root to Collapse, reusing and fast-forwarding
//!    an existing Collapse repository from Rosen's base instead of creating a new one.
//! 3. Set Issues on Collapse (#589) and CI from Rosen (#591).
//! 4. Optionally keep GitHub as a one-way downstream copy (C4), or remove it.
//! 5. Run `rosen collapse check` and report every step's result.

use std::time::Duration;

use tauri::AppHandle;
use velocity_collapse::{
    CollapseCredentials, GitHubIssueImportResult, MirrorRequest, ProvisionedMirror,
    RepositoryActivity,
};
use velocity_core::collapse::{
    CollapseCheckItemStatus, CollapseCheckSummary, ForwarderState, IssueHost, MoveToCollapseStepId,
    MoveToCollapseStepResult, MoveToCollapseStepStatus, downstream_settings_page,
    github_repository_from_remote, repository_name_for,
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
    finish_downstream_setup, load_credentials, mirror_subject, read_project_collapse_settings,
    refresh_health, refusal_reason, relay_note, saved_relay_allow, toast, typed_repository,
};

/// How long Move to Collapse waits for Rosen's background push to finish fast-forwarding
/// an existing Collapse repository from Rosen's base.
const MIRROR_SYNC_WAIT: Duration = Duration::from_secs(120);

/// Input for `move_to_collapse` (§3.1, #592).
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
}

/// Step-by-step outcome of `move_to_collapse` (§3.1, #592).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, specta::Type)]
pub struct MoveToCollapseResult {
    pub ok: bool,
    pub collapse_repository: Option<String>,
    pub github_repository: Option<String>,
    pub steps: Vec<MoveToCollapseStepResult>,
    pub collapse_check: Option<CollapseCheckSummary>,
    pub settings: ProjectCollapseSettings,
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
    let provisioned = velocity_collapse::ensure_mirror(
        &credentials,
        &MirrorRequest {
            owner: Some(&target_owner),
            name: &target_name,
            default_branch: &subject.branch,
            description: "",
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let collapse_repository = provisioned.repository();
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
    let ok = steps
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
    })
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
    let import_step = step_import_issues(
        provisioned,
        github_repository,
        request.github_import_token.as_deref(),
    )
    .await;
    let import_failed = import_step.status == MoveToCollapseStepStatus::Failed;
    steps.push(import_step);
    if import_failed {
        let reason =
            "Waiting for GitHub issue import to succeed; fix Step 1 and re-run Move to Collapse.";
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
            step_set_issues_and_ci(application, request.project_identifier, provisioned).await?,
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
    if let Some(explicit) = request.github_repository.as_deref()
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
    let from_health = crate::state::with_store(application, move |store| {
        store
            .project(project)
            .and_then(|item| item.collapse_mirror_health.as_ref())
            .and_then(|health| health.downstream_github.clone())
    })
    .await
    .ok()
    .flatten();
    if from_health.is_some() {
        return Ok(from_health);
    }
    let path = subject.repository_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        velocity_github::checkout_repository(&path).map(|name| name.full_name())
    })
    .await
    .map_err(|error| error.to_string())
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

async fn step_import_issues(
    provisioned: &ProvisionedMirror,
    github_repository: Option<&str>,
    github_import_token: Option<&str>,
) -> MoveToCollapseStepResult {
    let title = "Import GitHub issues".to_owned();
    let Some(github) = github_repository else {
        return skipped_step(
            MoveToCollapseStepId::ImportIssues,
            &title,
            "No GitHub repository configured for issue import.",
        );
    };
    let token = tauri::async_runtime::spawn_blocking(velocity_collapse::resolve_issues_token)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    let Some(token) = token else {
        return failed_step(
            MoveToCollapseStepId::ImportIssues,
            &title,
            "No Collapse token available for issue import.".into(),
            Some("Save a Collapse personal access token with the 'issues' scope in Collapse settings, then re-run Move to Collapse.".into()),
            None,
            None,
        );
    };
    match velocity_collapse::import_github_issues_and_wait(
        &token,
        &provisioned.owner,
        &provisioned.name,
        github,
        github_import_token,
    )
    .await
    {
        Ok(status) => {
            let (summary, detail) = format_import_summary(github, status.result.as_ref());
            MoveToCollapseStepResult {
                step: MoveToCollapseStepId::ImportIssues,
                title,
                status: MoveToCollapseStepStatus::Passed,
                summary,
                detail,
                action_url: None,
                action_label: None,
            }
        }
        Err(failure) => {
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
            failed_step(
                MoveToCollapseStepId::ImportIssues,
                &title,
                failure.message,
                detail,
                failure.action_url,
                action_label,
            )
        }
    }
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
    crate::rosen_mirror::refresh(application, project).await?;
    Ok(format_mirror_step(
        title,
        &repo,
        provisioned.created,
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
    created: bool,
    comparison: &RemoteBaseComparison,
    sync: MirrorFastForwardOutcome,
    landed_note: String,
) -> MoveToCollapseStepResult {
    match sync {
        MirrorFastForwardOutcome::Synced { base_commit } => {
            let tip = base_commit.as_deref().map(short_commit).unwrap_or("base");
            let summary = match (created, comparison) {
                (true, _) => format!("Created {repo} and pushed Rosen's base ({tip})."),
                (false, RemoteBaseComparison::BehindBy { commits_behind, .. }) => format!(
                    "Linked existing {repo} and fast-forwarded {commits_behind} {} to {tip}.",
                    if *commits_behind == 1 {
                        "commit"
                    } else {
                        "commits"
                    }
                ),
                (false, RemoteBaseComparison::EmptyRemote { .. }) => {
                    format!("Linked existing empty {repo} and pushed Rosen's base ({tip}).")
                }
                (false, RemoteBaseComparison::UpToDate { .. }) => {
                    format!("Linked existing {repo} (already up to date at {tip}).")
                }
                (false, _) => format!("Linked existing {repo} and synced Rosen's base ({tip})."),
            };
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
                "Linked {repo} in rosen.toml, but this Rosen server is not marked as mirror owner."
            ),
            Some(landed_note),
            None,
            None,
        ),
        MirrorFastForwardOutcome::Rejected { error } => failed_step(
            MoveToCollapseStepId::SwitchMirror,
            &title,
            format!("Linked {repo}, but Rosen's mirror push was rejected: {error}"),
            Some(landed_note),
            None,
            None,
        ),
        MirrorFastForwardOutcome::StillPushing { error } => MoveToCollapseStepResult {
            step: MoveToCollapseStepId::SwitchMirror,
            title,
            status: MoveToCollapseStepStatus::Warning,
            summary: format!("Linked {repo}; Rosen is still pushing the base in the background."),
            detail: Some(match error {
                Some(message) => format!("{landed_note} · {message}"),
                None => landed_note,
            }),
            action_url: None,
            action_label: None,
        },
    }
}

async fn step_set_issues_and_ci(
    application: &AppHandle,
    project: u32,
    provisioned: &ProvisionedMirror,
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
    crate::base_ci::wake(application);
    let has_issues_token =
        tauri::async_runtime::spawn_blocking(velocity_collapse::load_issues_token)
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
            .is_some();
    let detail = if has_issues_token {
        Some("Dispatch, AIRC issue tools, base CI repairs, release gate and pin tracking now run against Collapse and Rosen.".into())
    } else {
        Some("Issues and CI switched to Collapse and Rosen. Save a Collapse PAT with 'issues' scope in Collapse settings below to allow commenting and closing issues.".into())
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
    if let Some(row) = dashboard.iter().find(|row| row.is(&collapse_repo))
        && ForwarderState::from_collapse(&row.forward.state) != ForwarderState::Unconfigured
        && row
            .downstream_remote()
            .as_deref()
            .and_then(github_repository_from_remote)
            .is_some_and(|existing| existing.eq_ignore_ascii_case(github))
    {
        return Ok(MoveToCollapseStepResult {
            step: MoveToCollapseStepId::ConfigureDownstream,
            title,
            status: MoveToCollapseStepStatus::Passed,
            summary: format!(
                "Keeping one-way GitHub downstream copy: Collapse already forwards to {github} ({}).",
                row.forward.state
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
        } => MoveToCollapseStepResult {
            step: MoveToCollapseStepId::ConfigureDownstream,
            title,
            status: MoveToCollapseStepStatus::Warning,
            summary: format!(
                "Finish enabling GitHub downstream forwarding to {github_repository} on Collapse."
            ),
            detail: Some(reason.clone()),
            action_url: page_url.clone(),
            action_label: Some("Finish on Collapse".into()),
        },
    };
    let _ = finish_downstream_setup(application, project, &collapse_repo, Some(result));
    Ok(step)
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
