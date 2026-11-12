//! Headless resume-after-restart driver (SPEC.md §3.2, #279) against a real ACP agent.
//! Arguments: app-data worktree agent-program [--no-load]
//!
//! The agent starts a slow command; while it runs, the driver saves the thread the way the
//! app does and ends the session, as closing Velocity would. It then reloads `state.json` as
//! a fresh start does, finds the cut-off turn, and continues it through `session/load` (or,
//! with `--no-load`, a new session sent the transcript) with no message from you. The agent
//! has to finish the work it was given before the restart.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_client_protocol::v1::schema::SessionUpdate;
use velocity_acp::client::{AgentEvent, AgentLaunch, SessionResume, SessionSetup};
use velocity_store::{AgentActivity, PersistedState, ProjectItem, StateStore, ThreadItem};
use velocity_threads::parking::transcript_context;
use velocity_threads::restart::{CONTINUE_PROMPT, interrupted_turns};
use velocity_threads::{RunningSession, ThreadKey, ThreadRuntimes, spawn_session};

type Failure = Box<dyn std::error::Error>;

const KEY: ThreadKey = (1, 2);
/// A thread whose turn had ended before the exit: it must stay idle.
const IDLE: ThreadKey = (1, 3);
const WORD: &str = "heron";
const FIRST_PROMPT: &str = "Run the shell command `ping -n 20 127.0.0.1` and wait for it to finish. After it finishes, create a file named done.txt in the current directory containing only the word heron. Do nothing else.";

fn main() -> Result<(), Failure> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [data, worktree, program, rest @ ..] = arguments.as_slice() else {
        return Err("Pass app-data worktree agent-program [--no-load]".into());
    };
    let load = !rest.iter().any(|argument| argument == "--no-load");
    let data = PathBuf::from(data);
    let worktree = PathBuf::from(worktree);
    let agent = AgentLaunch::new(PathBuf::from(program));
    let _ = std::fs::remove_file(worktree.join("done.txt"));
    seed_state(&data, &worktree)?;

    let mut runtimes = ThreadRuntimes::default();
    let path = worktree.display().to_string();
    let inputs = runtimes.start(KEY, &path, FIRST_PROMPT);
    runtimes.worktree_ready(KEY);
    let session = spawn_session(agent.clone(), setup(&worktree, None), inputs);
    let session_id = run_until_mid_tool(&mut runtimes, session, &data)?;
    if worktree.join("done.txt").exists() {
        return Err("The agent finished before the simulated exit".into());
    }

    // A fresh start: nothing in memory survives, only state.json.
    let mut store = StateStore::load_or_default(&data)?;
    let interrupted = interrupted_turns(store.state());
    println!("interrupted turns found on start: {interrupted:?}");
    if interrupted != [KEY] {
        return Err("Expected exactly the cut-off thread, not the idle one".into());
    }
    store.reset_disconnected_threads()?;
    let mut runtimes = ThreadRuntimes::default();
    runtimes.restore(store.state());
    let blocks = runtimes
        .view(KEY)
        .map(|view| view.blocks)
        .unwrap_or_default();
    let earlier = Some(session_id).filter(|_| load);
    let resume = SessionResume::new(earlier, transcript_context(&blocks))
        .with_history_replay(blocks.is_empty());
    let inputs = runtimes.resume_parked(KEY, &path, CONTINUE_PROMPT, false)?;
    let session = spawn_session(agent, setup(&worktree, Some(resume)), inputs);
    finish_resumed(&mut runtimes, session)?;
    let written = std::fs::read_to_string(worktree.join("done.txt"))?;
    println!("done.txt after the restart: {:?}", written.trim());
    if written.trim() != WORD {
        return Err("The resumed agent did not finish its earlier work".into());
    }
    println!("The cut-off turn continued by itself after the restart");
    Ok(())
}

fn setup(worktree: &std::path::Path, resume: Option<SessionResume>) -> SessionSetup {
    let mut setup = SessionSetup::new(worktree.to_path_buf(), vec!["bypassPermissions".into()]);
    setup.resume = resume;
    setup
}

/// `state.json` with the working thread and a second thread whose turn had ended.
fn seed_state(data: &std::path::Path, worktree: &std::path::Path) -> Result<(), Failure> {
    std::fs::create_dir_all(data)?;
    let mut project = ProjectItem::new(1, "Restart driver".into(), worktree.display().to_string());
    project.spec_locked = true;
    for (identifier, title) in [(KEY.1, "Cut off"), (IDLE.1, "Finished")] {
        let mut thread = ThreadItem::new(identifier, title.into());
        thread.worktree_path = Some(worktree.display().to_string());
        thread.branch_name = format!("vel/restart-{identifier}");
        thread.activity = AgentActivity::Idle;
        thread.agent_session = Some("earlier".into());
        project.threads.push(thread);
    }
    let mut state = PersistedState::default();
    state.projects.push(project);
    std::fs::write(data.join("state.json"), serde_json::to_vec(&state)?)?;
    Ok(())
}

/// Runs the first session until its command has run a few seconds, saves the thread as the
/// app's event pump does, then ends the session as closing the app would.
fn run_until_mid_tool(
    runtimes: &mut ThreadRuntimes,
    mut session: RunningSession,
    data: &std::path::Path,
) -> Result<String, Failure> {
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut session_id = None;
    let mut tool_started = None;
    while tool_started.is_none_or(|started: Instant| started.elapsed() < Duration::from_secs(4)) {
        if Instant::now() > deadline {
            return Err("The agent never started its command".into());
        }
        while let Ok(event) = session.events.try_recv() {
            if let AgentEvent::SessionStarted { session_id: id, .. } = &event {
                session_id = Some(id.0.to_string());
            }
            if let AgentEvent::Transcript {
                update: SessionUpdate::ToolCall(call),
                ..
            } = &event
            {
                println!("tool call started: {}", call.title);
                tool_started.get_or_insert_with(Instant::now);
            }
            if let AgentEvent::TurnEnded(outcome) = &event {
                return Err(format!("The turn ended before the exit: {outcome:?}").into());
            }
            runtimes.apply(KEY, &event);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let session_id = session_id.ok_or("The agent reported no session")?;
    let transcript = runtimes.view(KEY).map(|view| view.blocks);
    let mut store = StateStore::load_or_default(data)?;
    store.update_thread(KEY.0, KEY.1, |thread| {
        thread.activity = AgentActivity::Executing;
        thread.agent_session = Some(session_id.clone());
        thread.transcript = transcript.unwrap_or_default();
    })?;
    drop(store);
    if let Some(ended) = runtimes.park(KEY) {
        while let Some(event) = session.events.blocking_recv() {
            runtimes.apply(KEY, &event);
        }
        let _ = session.outcome.blocking_recv();
        runtimes.session_ended_with_shutdown(KEY, session.shutdown.confirmed());
        ended.blocking_recv()?;
    }
    println!(
        "simulated exit mid-command: session {session_id} ended, processes stopped: {}",
        session.shutdown.confirmed()
    );
    Ok(session_id)
}

fn finish_resumed(
    runtimes: &mut ThreadRuntimes,
    mut session: RunningSession,
) -> Result<(), Failure> {
    while let Some(event) = session.events.blocking_recv() {
        match &event {
            AgentEvent::SessionStarted { session_id, loaded } => {
                println!("resumed session {} (loaded: {loaded})", session_id.0);
            }
            AgentEvent::Transcript {
                update: SessionUpdate::ToolCall(call),
                ..
            } => println!("resumed tool call: {}", call.title),
            AgentEvent::TurnEnded(outcome) => {
                println!("resumed turn ended: {outcome:?}");
                runtimes.apply(KEY, &event);
                break;
            }
            _ => {}
        }
        runtimes.apply(KEY, &event);
    }
    runtimes.stop(KEY);
    while let Some(event) = session.events.blocking_recv() {
        runtimes.apply(KEY, &event);
    }
    Ok(())
}
