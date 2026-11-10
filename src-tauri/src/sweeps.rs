//! Scout sweeps at the Tauri edge (`SPEC.md Â§4 F7`, `Â§7 M5`): starts the sweep runner, saves
//! and publishes what it reports, and serves the scout MCP server when Velocity's own
//! executable is started as one.

use std::io::{BufReader, Write};
use std::path::PathBuf;

use anyhow::Context;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_core::duty::DutyRole;
use velocity_github::{IssueClient, ScoutServer};
use velocity_interview::Interviews;
use velocity_store::duty::ScoutRoster;
use velocity_threads::{SCOUT_SERVER_SWITCH, ScoutChange, ScoutSweeps, SweepHost};

use crate::duty::DutyEventPublished;
use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// `velocity --scout-mcp <worktree> <scout> <report>`: serve one scout's MCP connection on
/// stdin/stdout and exit. `false` when Velocity was started normally.
pub fn serve_scout_server() -> anyhow::Result<bool> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) != Some(SCOUT_SERVER_SWITCH) {
        return Ok(false);
    }
    let [_, worktree, scout, report] = arguments.as_slice() else {
        anyhow::bail!("Usage: {SCOUT_SERVER_SWITCH} <worktree> <scout> <report-file>");
    };
    let client = IssueClient::discover(std::path::Path::new(worktree)).map_err(|error| {
        format!("This project has no GitHub repository to file issues in ({error})")
    });
    let mut server = ScoutServer::new(client, scout.clone(), Some(PathBuf::from(report)));
    let input = BufReader::new(std::io::stdin().lock());
    let mut output = std::io::stdout().lock();
    server
        .serve(input, &mut output)
        .context("The scout MCP connection failed")?;
    output.flush()?;
    Ok(true)
}

/// Starts the runner for the life of the app.
pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let executable = std::env::current_exe().context("Could not find Velocity's executable")?;
    let reports = application
        .path()
        .app_data_dir()
        .context("Could not resolve the application data directory")?
        .join("scout-reports");
    std::fs::create_dir_all(&reports).context("Could not create the scout report folder")?;
    let sweeps = ScoutSweeps::new(executable, reports);
    application.manage(sweeps.clone());
    let host = TauriSweepHost(application.clone());
    tauri::async_runtime::spawn(sweeps.run(host));
    Ok(())
}

/// Scouts were started, added, paused or resumed: look for due sweeps now.
pub fn wake(application: &AppHandle) {
    if let Some(sweeps) = application.try_state::<ScoutSweeps>() {
        sweeps.wake();
    }
}

/// Your message from the Scouts view's composer goes to every scout (F7).
pub fn instruct(application: &AppHandle, role: DutyRole, instruction: &str) {
    if role != DutyRole::Scouts {
        return;
    }
    let project = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .map(|store| store.active_project().map(|project| project.id));
    if let (Ok(Some(project)), Some(sweeps)) = (project, application.try_state::<ScoutSweeps>()) {
        sweeps.instruct(project, instruction);
    }
}

#[derive(Clone)]
struct TauriSweepHost(AppHandle);

impl SweepHost for TauriSweepHost {
    fn rosters(&self) -> Vec<ScoutRoster> {
        lock(
            self.0.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .map(|store| store.scout_rosters())
        .unwrap_or_default()
    }

    fn update(&self, project: u32, change: ScoutChange) -> bool {
        let saved = self.save(project, change);
        report(saved.clone().map(|_| ()));
        saved.unwrap_or(false)
    }
}

impl TauriSweepHost {
    /// Saves, then republishes the window when the project is the one on screen.
    fn save(&self, project: u32, change: ScoutChange) -> Result<bool, String> {
        let store_state = self.0.state::<ManagedStateStore>();
        let mut store = lock(store_state.inner(), "persisted state")?;
        let Some(events) = store
            .update_scouts(project, change)
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        if store.active_project().map(|item| item.id) != Some(project) {
            drop(store);
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
                role: DutyRole::Scouts,
                event,
            }
            .emit(&self.0)
            .map_err(|error| format!("Could not publish duty event: {error}"))?;
        }
        Ok(true)
    }
}
