//! Runs a follow-up in an existing worktree, records delivery and stops its owned processes.
//! Usage: follow_up_session <worktree> <message|--stop-on-log> <agent executable> [args...]
use std::path::PathBuf;
use velocity_acp::client::{AgentEvent, AgentLaunch, SessionSetup};
use velocity_threads::{
    DeliveryStatus, RunningSession, StopOutcome, ThreadRuntimes, spawn_session,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let worktree = PathBuf::from(arguments.next().ok_or("expected worktree")?);
    let message = arguments
        .next()
        .ok_or("expected message or --stop-on-log")?;
    let mut agent = AgentLaunch::new(arguments.next().ok_or("expected agent executable")?.into());
    agent.args = arguments.collect();
    let mut runtimes = ThreadRuntimes::default();
    let inputs =
        runtimes.resume_follow_through((1, 1), &worktree.to_string_lossy(), "manual", &message)?;
    let session = spawn_session(agent, SessionSetup::new(worktree, Vec::new()), inputs);
    run(&mut runtimes, session, &message)
}

fn run(
    runtimes: &mut ThreadRuntimes,
    mut session: RunningSession,
    message: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stopped = None;
    while let Some(event) = session.events.blocking_recv() {
        runtimes.apply((1, 1), &event);
        let finished = matches!(&event, AgentEvent::TurnEnded(_));
        let stop_on_log = message == "--stop-on-log" && matches!(&event, AgentEvent::Log(_));
        if let AgentEvent::Log(line) = &event {
            eprintln!("{line}");
        }
        if stopped.is_none() && (finished || stop_on_log) {
            if finished {
                let delivery = runtimes.deliver_follow_up((1, 1), "manual", message)?;
                println!("delivery: {delivery:?}");
                if delivery != DeliveryStatus::Delivered {
                    return Err("follow-up did not complete".into());
                }
                let work = runtimes
                    .work_snapshot((1, 1))
                    .ok_or("missing work history")?;
                println!("fix steps: {}", work.steps.len());
            }
            if let StopOutcome::Stopping(ended) = runtimes.stop((1, 1)) {
                stopped = Some(ended);
            }
        }
    }
    let outcome = session.outcome.blocking_recv()?;
    runtimes.session_ended_with_shutdown((1, 1), session.shutdown.confirmed());
    outcome?;
    let ended = stopped.ok_or("session ended without a stop acknowledgement")?;
    ended.blocking_recv()?;
    if runtimes.shutdown_failed((1, 1)) {
        return Err("owned process shutdown failed".into());
    }
    println!("shutdown: confirmed");
    Ok(())
}
