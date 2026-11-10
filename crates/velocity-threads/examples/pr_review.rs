//! F7 PR duty without a window: starts PR duty on the saved active project (if it has none),
//! runs the real review runner until every review it started has finished, and prints each
//! card change and feed event as the Duty view would show them.
//!
//! cargo run -p velocity-threads --example pr_review -- <state folder> <velocity executable>
//!     [provider] [instruction]
//! The executable is the app build (`target/debug/velocity-app.exe`), which serves the PR
//! duty MCP server when started with `--pr-duty-mcp`. The state folder doubles as the app
//! data folder, so the approval record lands in `<state folder>/airc`.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use velocity_core::duty::DutyRole;
use velocity_core::layout_state::LayoutState;
use velocity_store::StateStore;
use velocity_store::duty::{PrDutyRoster, StartDutyRequest};
use velocity_threads::{PrDutyReviews, ReviewChange, ReviewHost};

#[derive(Clone)]
struct Host(Arc<Mutex<StateStore>>);

impl ReviewHost for Host {
    fn rosters(&self) -> Vec<PrDutyRoster> {
        self.0
            .lock()
            .map(|store| store.pr_duty_rosters())
            .unwrap_or_default()
    }

    fn update(&self, project: u32, change: ReviewChange) -> bool {
        let Ok(mut store) = self.0.lock() else {
            return false;
        };
        let Ok(Some(events)) = store.update_duty(project, DutyRole::PrDuty, change) else {
            return false;
        };
        for event in events {
            println!(
                "feed  {} {} {:?} {}",
                event.timestamp, event.actor, event.kind, event.message
            );
        }
        for roster in store.pr_duty_rosters() {
            for agent in roster.agents {
                println!(
                    "card  {} busy={} doing={}",
                    agent.nickname, agent.busy, agent.doing
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
    if store.pr_duty_rosters().is_empty() {
        // The same request the start dialog sends: one PR duty agent with the default rules.
        let request: StartDutyRequest = serde_json::from_value(serde_json::json!({
            "role": "pr_duty", "agent_count": 1, "provider": provider, "model_name": null,
            "pr_rules": null, "scout_rules": null,
        }))?;
        store.start_or_add_duty(&mut LayoutState::default(), request, "00:00".into())?;
    }
    let host = Host(Arc::new(Mutex::new(store)));
    let reviews = PrDutyReviews::new(executable, folder);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        if let Some(instruction) = &instruction {
            reviews.instruct(project, instruction);
        }
        for look in ["first", "second"] {
            // The second look: a decided or attempted head is not claimed again.
            reviews.tick(&host).await;
            println!("claimed on the {look} look: {:?}", reviews.running());
            while !reviews.running().is_empty() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    });
    Ok(())
}
