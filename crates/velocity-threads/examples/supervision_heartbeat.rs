//! Runs the streaming heartbeat path without starting a provider process.
use velocity_acp::client::AgentEvent;
use velocity_store::{AgentActivity, Settings, ThreadItem};
use velocity_threads::{
    ThreadRuntimes,
    agent_activity::{self, Action, decide},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = (1, 1);
    let mut runtimes = ThreadRuntimes::default();
    let _inputs = runtimes.start(key, ".", "Work");
    let mut thread = ThreadItem::new(1, "Streaming agent".into());
    thread.activity = AgentActivity::Thinking;
    agent_activity::begin(&mut thread, 100);
    runtimes.observe_supervision(key, &[], 100);
    let mut checkpoints = 0;
    for timestamp in 101..=1000 {
        if runtimes.observe_supervision(
            key,
            &[AgentEvent::FileWritten {
                path: "output.rs".into(),
            }],
            timestamp,
        ) {
            checkpoints += 1;
        }
    }
    thread.supervision.last_update = runtimes.heartbeat(key).ok_or("Missing heartbeat")?;
    let settings = Settings::default().agents;
    if checkpoints != 0 || decide(&thread, &settings, 1001, true, false) != Action::Nothing {
        return Err("Streaming triggered a checkpoint or false stall".into());
    }
    if decide(&thread, &settings, 1300, true, false) != Action::Nudge {
        return Err("Silence did not trigger a nudge".into());
    }
    runtimes.nudge_supervision(key, 1300);
    if !runtimes.observe_supervision(
        key,
        &[AgentEvent::FileWritten {
            path: "output.rs".into(),
        }],
        1301,
    ) || !runtimes.observe_supervision(
        key,
        &[AgentEvent::TurnEnded(Ok(
            agent_client_protocol::v1::schema::StopReason::EndTurn,
        ))],
        1302,
    ) {
        return Err("Recovery or turn end checkpoint was lost".into());
    }
    println!(
        "900 streaming batches: {checkpoints} checkpoints; exact stall timeout, recovery and turn end retained"
    );
    Ok(())
}
