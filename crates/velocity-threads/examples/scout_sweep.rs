//! F7 scout sweeps without a window: starts Scouts on the saved active project (if it has
//! none), runs the real sweep runner until every sweep it started has finished, and prints
//! each card change and feed event as the Duty view would show them.
//!
//! cargo run -p velocity-threads --example scout_sweep -- <state folder> <velocity executable>
//!     [provider] [instruction]
//! The executable is the app build (`target/debug/velocity-app.exe`), which serves the scout
//! MCP server when started with `--scout-mcp`.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use velocity_core::layout_state::LayoutState;
use velocity_store::StateStore;
use velocity_store::duty::{ScoutRoster, StartDutyRequest};
use velocity_threads::{ScoutChange, ScoutSweeps, SweepHost};

#[derive(Clone)]
struct Host(Arc<Mutex<StateStore>>);

impl SweepHost for Host {
    fn rosters(&self) -> Vec<ScoutRoster> {
        self.0
            .lock()
            .map(|store| store.scout_rosters())
            .unwrap_or_default()
    }

    fn update(&self, project: u32, change: ScoutChange) -> bool {
        let Ok(mut store) = self.0.lock() else {
            return false;
        };
        let Ok(Some(events)) = store.update_scouts(project, change) else {
            return false;
        };
        for event in events {
            println!(
                "feed  {} {} {:?} {}",
                event.timestamp, event.actor, event.kind, event.message
            );
        }
        for roster in store.scout_rosters() {
            for scout in roster.scouts {
                println!(
                    "card  {} busy={} doing={}",
                    scout.nickname, scout.busy, scout.doing
                );
            }
        }
        true
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let folder = PathBuf::from(arguments.next().ok_or("missing <state folder>")?);
    let executable = PathBuf::from(arguments.next().ok_or("missing <velocity executable>")?);
    let provider = arguments.next().unwrap_or_else(|| "kimi".into());
    let instruction = arguments.next();
    let mut store = StateStore::load_or_default(folder.clone())?;
    let project = store.active_project().ok_or("no active project")?.id;
    if store.scout_rosters().is_empty() {
        // The same request the start dialog sends: one scout, sweeping once.
        let request: StartDutyRequest = serde_json::from_value(serde_json::json!({
            "role": "scouts", "agent_count": 1, "provider": provider, "model_name": null,
            "pr_rules": null,
            "scout_rules": {"focus_areas": ["bugs"], "assume_every_part_fails": true,
                "run_stress_against_models": false, "label_issues_for_dispatch": false,
                "cadence": "once"},
        }))?;
        store.start_or_add_duty(&mut LayoutState::default(), request, "00:00".into())?;
    }
    let host = Host(Arc::new(Mutex::new(store)));
    let sweeps = ScoutSweeps::new(executable, folder.join("scout-reports"));
    std::fs::create_dir_all(folder.join("scout-reports"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        if let Some(instruction) = &instruction {
            sweeps.instruct(project, instruction);
        }
        sweeps.tick(&host).await;
        while !sweeps.running().is_empty() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // A second look: the cadence decides whether anything sweeps again.
        sweeps.tick(&host).await;
        println!("running after the second tick: {:?}", sweeps.running());
    });
    Ok(())
}
