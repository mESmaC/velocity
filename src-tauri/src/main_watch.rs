//! The Main watcher at the Tauri edge (`SPEC.md §4 F7`, `§7 M10`): starts the watch runner,
//! saves and publishes what it reports, and serves the watch MCP server when Velocity's own
//! executable is started as one.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_airc::Channel;
use velocity_core::duty::DutyRole;
use velocity_github::{PullRequestClient, WatchServer, WatchTarget};
use velocity_interview::Interviews;
use velocity_store::duty::WatchRoster;
use velocity_threads::{MainWatches, WATCH_SERVER_SWITCH, WatchChange, WatchHost};

use crate::duty::DutyEventPublished;
use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// `velocity --watch-mcp <worktree> <app-data> <project> <watcher> <base> <tip> <report>`:
/// serve one watch's MCP connection on stdin/stdout and exit. `false` when Velocity was
/// started normally.
pub fn serve_watch_server() -> anyhow::Result<bool> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) != Some(WATCH_SERVER_SWITCH) {
        return Ok(false);
    }
    let [_, worktree, app_data, project, watcher, base, tip, report] = arguments.as_slice() else {
        anyhow::bail!(
            "Usage: {WATCH_SERVER_SWITCH} <worktree> <app-data> <project> <watcher> <base-branch> <tip> <report-file>"
        );
    };
    // The watcher's nick and tip come from the launcher, so a verdict can't be recorded as
    // anyone else or about another tip.
    let channel = Channel::open(Path::new(app_data), project)
        .map_err(|error| format!("This watch has no AIRC channel ({error})"));
    let pull_requests = PullRequestClient::discover(Path::new(worktree))
        .map_err(|error| format!("This project has no GitHub repository ({error})"));
    let target = WatchTarget::new(
        PathBuf::from(worktree),
        watcher.clone(),
        base.clone(),
        tip.clone(),
    );
    let mut server = WatchServer::new(target, channel, pull_requests, Some(PathBuf::from(report)));
    let input = BufReader::new(std::io::stdin().lock());
    let mut output = std::io::stdout().lock();
    server
        .serve(input, &mut output)
        .context("The watch MCP connection failed")?;
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
    let reports = app_data.join("watch-reports");
    std::fs::create_dir_all(&reports).context("Could not create the watch report folder")?;
    let watches = MainWatches::new(executable, app_data, reports)
        .with_airc(crate::airc_access::launch(application)?);
    application.manage(watches.clone());
    let host = TauriWatchHost(application.clone());
    tauri::async_runtime::spawn(watches.run(host));
    Ok(())
}

/// The Main watcher was started, added to, paused or resumed: look for a new tip now.
pub fn wake(application: &AppHandle) {
    if let Some(watches) = application.try_state::<MainWatches>() {
        watches.wake();
    }
}

/// Your message from the Main watcher view's composer goes to the watcher (F7).
pub fn instruct(application: &AppHandle, role: DutyRole, instruction: &str) {
    if role != DutyRole::MainWatcher {
        return;
    }
    let project = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .map(|store| store.active_project().map(|project| project.id));
    if let (Ok(Some(project)), Some(watches)) = (project, application.try_state::<MainWatches>()) {
        watches.instruct(project, instruction);
    }
}

#[derive(Clone)]
struct TauriWatchHost(AppHandle);

impl WatchHost for TauriWatchHost {
    fn rosters(&self) -> Vec<WatchRoster> {
        lock(
            self.0.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .map(|store| store.main_watch_rosters())
        .unwrap_or_default()
    }

    fn update(&self, project: u32, change: WatchChange) -> bool {
        let saved = self.save(project, change);
        report(saved.clone().map(|_| ()));
        saved.unwrap_or(false)
    }
}

impl TauriWatchHost {
    /// Saves, then republishes the window when the project is the one on screen.
    fn save(&self, project: u32, change: WatchChange) -> Result<bool, String> {
        let store_state = self.0.state::<ManagedStateStore>();
        let mut store = lock(store_state.inner(), "persisted state")?;
        let Some(events) = store
            .update_duty(project, DutyRole::MainWatcher, change)
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        if store.active_project().map(|item| item.id) != Some(project) {
            drop(store);
            // Flaky-test filings post `issue` events like a scout's (F14).
            crate::airc_events::record_scout_events(&self.0, project, &events);
            return Ok(true);
        }
        let mut layout = lock(self.0.state::<MainLayout>().inner(), "main-window state")?;
        store.refresh_layout(&mut layout);
        velocity_interview::overlay_live(self.0.state::<Interviews>().inner(), &store, &mut layout);
        let snapshot = layout.snapshot();
        // Publish under the locks so snapshots reach the window in order.
        crate::layout::publish(&self.0, &snapshot, None)?;
        drop(layout);
        drop(store);
        crate::airc_events::record_scout_events(&self.0, project, &events);
        for event in events {
            DutyEventPublished {
                role: DutyRole::MainWatcher,
                event,
            }
            .emit(&self.0)
            .map_err(|error| format!("Could not publish duty event: {error}"))?;
        }
        Ok(true)
    }
}
