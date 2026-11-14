//! PR duty reviews at the Tauri edge (`SPEC.md §4 F7`, `§7 M5`): starts the review runner,
//! saves and publishes what it reports, and serves the PR duty MCP server when Velocity's own
//! executable is started as one.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_core::duty::DutyRole;
use velocity_github::{MergeMethod, PrDutyServer, PullRequestClient, ReviewRecord};
use velocity_interview::Interviews;
use velocity_store::duty::PrDutyRoster;
use velocity_threads::{
    PR_DUTY_SERVER_SWITCH, PrDutyReviews, READING_REVIEW_ARGUMENT, ReviewChange, ReviewHost,
};

use crate::duty::DutyEventPublished;
use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// `velocity --pr-duty-mcp <worktree> <app-data> <project> <nick> <number> <method> <report>`,
/// then `reading <base-commit>` for a review by reading: serve one review's MCP connection on
/// stdin/stdout and exit. `false` when Velocity was started normally.
pub fn serve_pr_duty_server() -> anyhow::Result<bool> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) != Some(PR_DUTY_SERVER_SWITCH) {
        return Ok(false);
    }
    let [
        _,
        worktree,
        app_data,
        project,
        nick,
        number,
        method,
        report,
        reading @ ..,
    ] = arguments.as_slice()
    else {
        anyhow::bail!(
            "Usage: {PR_DUTY_SERVER_SWITCH} <worktree> <app-data> <project> <nick> <number> <merge-method> <report-file> [{READING_REVIEW_ARGUMENT} <base-commit>]"
        );
    };
    let number: u32 = number.parse().context("The PR number is not a number")?;
    let method = match method.as_str() {
        "rebase" => MergeMethod::Rebase,
        "merge" => MergeMethod::Merge,
        _ => MergeMethod::Squash,
    };
    // The reviewer's nick comes from the launcher, so decisions can't be recorded as anyone else.
    let client = ReviewRecord::open(Path::new(app_data), project, nick)
        .and_then(|record| {
            PullRequestClient::discover(Path::new(worktree))
                .map(|client| client.with_review_record(record))
        })
        .map_err(|error| format!("This review has no GitHub PR access ({error})"));
    let mut server = PrDutyServer::new(
        client,
        nick.clone(),
        number,
        method,
        Some(PathBuf::from(report)),
    )
    .with_statement_lock(
        PathBuf::from(worktree),
        PathBuf::from(app_data),
        project.clone(),
    );
    if let [mode, base_commit] = reading
        && mode == READING_REVIEW_ARGUMENT
    {
        server = server.reading(base_commit.clone());
    }
    let input = BufReader::new(std::io::stdin().lock());
    let mut output = std::io::stdout().lock();
    server
        .serve(input, &mut output)
        .context("The PR duty MCP connection failed")?;
    output.flush()?;
    Ok(true)
}

/// Starts the runner for the life of the app.
pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let executable = std::env::current_exe().context("Could not find Velocity's executable")?;
    let app_data = application
        .path()
        .app_data_dir()
        .context("Could not resolve the application data directory")?;
    let reviews = PrDutyReviews::new(executable, app_data)
        .with_airc(crate::airc_access::launch(application)?);
    application.manage(reviews.clone());
    let host = TauriReviewHost(application.clone());
    tauri::async_runtime::spawn(reviews.run(host));
    Ok(())
}

/// PR duty was started, added to, paused or resumed: look for PRs to claim now.
pub fn wake(application: &AppHandle) {
    if let Some(reviews) = application.try_state::<PrDutyReviews>() {
        reviews.wake();
    }
}

/// Your message from the PR duty view's composer goes to every agent in the role (F7).
pub fn instruct(application: &AppHandle, role: DutyRole, instruction: &str) {
    if role != DutyRole::PrDuty {
        return;
    }
    let project = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .map(|store| store.active_project().map(|project| project.id));
    if let (Ok(Some(project)), Some(reviews)) = (project, application.try_state::<PrDutyReviews>())
    {
        reviews.instruct(project, instruction);
    }
}

#[derive(Clone)]
struct TauriReviewHost(AppHandle);

impl ReviewHost for TauriReviewHost {
    fn rosters(&self) -> Vec<PrDutyRoster> {
        lock(
            self.0.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .map(|store| store.pr_duty_rosters())
        .unwrap_or_default()
    }

    fn update(&self, project: u32, change: ReviewChange) -> bool {
        let saved = self.save(project, change);
        report(saved.clone().map(|_| ()));
        saved.unwrap_or(false)
    }
}

impl TauriReviewHost {
    /// Saves, then republishes the window when the project is the one on screen.
    fn save(&self, project: u32, change: ReviewChange) -> Result<bool, String> {
        let store_state = self.0.state::<ManagedStateStore>();
        let mut store = lock(store_state.inner(), "persisted state")?;
        let Some(events) = store
            .update_duty(project, DutyRole::PrDuty, change)
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        if store.active_project().map(|item| item.id) != Some(project) {
            return Ok(true);
        }
        let mut layout = lock(self.0.state::<MainLayout>().inner(), "main-window state")?;
        store.refresh_layout(&mut layout);
        velocity_interview::overlay_live(self.0.state::<Interviews>().inner(), &store, &mut layout);
        let snapshot = layout.snapshot();
        // Publish under the locks so snapshots reach the window in order.
        crate::layout::publish(&self.0, &snapshot, None)?;
        for event in events {
            DutyEventPublished {
                role: DutyRole::PrDuty,
                event,
            }
            .emit(&self.0)
            .map_err(|error| format!("Could not publish duty event: {error}"))?;
        }
        Ok(true)
    }
}
