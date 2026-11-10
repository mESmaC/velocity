//! F7 Main watcher without a window: starts the Main watcher on the saved active project (if
//! it has none), runs the real watch runner until the watch it started has finished, and
//! prints each card change and feed event as the Duty view would show them. Run it again
//! after a merge to watch the new tip: the saved card remembers the last watched commit.
//!
//! cargo run -p velocity-threads --example main_watch -- <state folder> <velocity executable>
//!     [provider] [instruction]
//! The executable is the app build (`target/debug/velocity-app.exe`), which serves the watch
//! and scout MCP servers. The state folder doubles as the app data folder, so `broken` events
//! land in `<state folder>/airc`.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use velocity_core::duty::DutyRole;
use velocity_core::layout_state::LayoutState;
use velocity_store::StateStore;
use velocity_store::duty::{StartDutyRequest, WatchRoster};
use velocity_threads::{MainWatches, WatchChange, WatchHost};

#[derive(Clone)]
struct Host(Arc<Mutex<StateStore>>);

impl WatchHost for Host {
    fn rosters(&self) -> Vec<WatchRoster> {
        self.0
            .lock()
            .map(|store| store.main_watch_rosters())
            .unwrap_or_default()
    }

    fn update(&self, project: u32, change: WatchChange) -> bool {
        let Ok(mut store) = self.0.lock() else {
            return false;
        };
        let Ok(Some(events)) = store.update_duty(project, DutyRole::MainWatcher, change) else {
            return false;
        };
        for event in events {
            println!(
                "feed  {} {} {:?} {}",
                event.timestamp, event.actor, event.kind, event.message
            );
        }
        for roster in store.main_watch_rosters() {
            for watcher in roster.watchers {
                println!(
                    "card  {} busy={} doing={}",
                    watcher.nickname, watcher.busy, watcher.doing
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
    let provider = arguments.next().unwrap_or_else(|| "claude".into());
    let instruction = arguments.next();
    let mut store = StateStore::load_or_default(folder.clone())?;
    let project = store.active_project().ok_or("no active project")?.id;
    if store.main_watch_rosters().is_empty() {
        // The same request the start dialog sends: one watcher on the default model.
        let request: StartDutyRequest = serde_json::from_value(serde_json::json!({
            "role": "main_watcher", "agent_count": 1, "provider": provider,
            "model_name": null, "pr_rules": null, "scout_rules": null,
        }))?;
        store.start_or_add_duty(&mut LayoutState::default(), request, "00:00".into())?;
    }
    let host = Host(Arc::new(Mutex::new(store)));
    let reports = folder.join("watch-reports");
    std::fs::create_dir_all(&reports)?;
    let watches = MainWatches::new(executable, folder, reports);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        if let Some(instruction) = &instruction {
            watches.instruct(project, instruction);
        }
        for look in ["first", "second"] {
            // The second look: a tip already watched is not watched again.
            watches.tick(&host).await;
            println!("watching on the {look} look: {:?}", watches.running());
            while !watches.running().is_empty() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    });
    Ok(())
}
