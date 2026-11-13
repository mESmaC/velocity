//! Headless parking driver (SPEC.md §2.2) against a real ACP agent.
//! Arguments: app-data worktree airc-server agent-program [--no-load]
//!
//! The agent calls `airc_wait`; Velocity parks it, ending its session while the call is
//! pending. The holder's PR then closes, and the agent resumes through `session/load` (or,
//! with `--no-load`, a new session sent the transcript) and answers a message that was
//! queued while it was parked.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use velocity_acp::client::{AgentEvent, AgentLaunch, AircLaunch, SessionResume, SessionSetup};
use velocity_airc::Channel;
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::{AgentActivity, ThreadState};
use velocity_threads::parking::{
    ParkingAction, ParkingInput, parking_action, resume_prompt, transcript_context,
};
use velocity_threads::{RunningSession, ThreadKey, ThreadRuntimes, spawn_session};

type Failure = Box<dyn std::error::Error>;

const KEY: ThreadKey = (1, 2);
const MEMBER: &str = "thread-2";
const FIRST_PROMPT: &str = "Remember the number 4471. Then call the airc_wait tool of the airc MCP server with holder \"first\" and file \"shared.rs\". Do nothing else.";
const QUEUED: &str = "What number did I ask you to remember? Reply with only the number.";

struct Driver {
    channel: Channel,
    runtimes: ThreadRuntimes,
    worktree: PathBuf,
    airc: AircLaunch,
    agent: AgentLaunch,
    session_id: Option<String>,
}

fn main() -> Result<(), Failure> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [data, worktree, server, program, rest @ ..] = arguments.as_slice() else {
        return Err("Pass app-data worktree airc-server agent-program [--no-load]".into());
    };
    let load = !rest.iter().any(|argument| argument == "--no-load");
    let channel = Channel::open(&PathBuf::from(data), "1")?;
    channel.join("thread-1", "first")?;
    channel.claim(
        "thread-1",
        "First piece",
        "vel/first",
        vec!["shared.rs".into()],
    )?;
    channel.join(MEMBER, "second")?;
    channel.claim(
        MEMBER,
        "Second piece",
        "vel/second",
        vec!["shared.rs".into()],
    )?;
    let mut driver = Driver {
        channel,
        runtimes: ThreadRuntimes::default(),
        worktree: PathBuf::from(worktree),
        airc: AircLaunch::new(PathBuf::from(server), PathBuf::from(data)),
        agent: AgentLaunch::new(PathBuf::from(program)),
        session_id: None,
    };
    let path = driver.worktree.display().to_string();
    let inputs = driver.runtimes.start(KEY, &path, FIRST_PROMPT);
    driver.runtimes.worktree_ready(KEY);
    let session = spawn_session(driver.agent.clone(), driver.setup(None), inputs);
    driver.park_on_wait(session)?;
    driver.runtimes.steer(KEY, QUEUED)?;
    println!("queued while parked: {QUEUED}");
    driver
        .channel
        .dependency_ended("thread-1", "closed", Some(42))?;
    driver.wake(load)?;
    for event in driver.channel.events(0)? {
        println!("airc {} {} {}", event.kind, event.nick, event.text);
    }
    Ok(())
}

impl Driver {
    fn setup(&self, resume: Option<SessionResume>) -> SessionSetup {
        let mut setup = SessionSetup::new(self.worktree.clone(), vec!["bypassPermissions".into()]);
        setup.airc = Some(self.airc.thread(KEY.0, KEY.1, &self.worktree));
        setup.resume = resume;
        setup
    }

    fn apply(&mut self, event: &AgentEvent) {
        if let AgentEvent::SessionStarted { session_id, loaded } = event {
            println!("session {} (loaded: {loaded})", session_id.0);
            self.session_id = Some(session_id.0.to_string());
        }
        self.runtimes.apply(KEY, event);
    }

    fn input(&self, activity: AgentActivity) -> Result<ParkingInput, Failure> {
        Ok(ParkingInput {
            state: ThreadState::Working,
            activity,
            session_running: self.runtimes.is_live(KEY),
            wait: self.channel.wait_state(MEMBER)?,
        })
    }

    fn park_on_wait(&mut self, mut session: RunningSession) -> Result<(), Failure> {
        let deadline = Instant::now() + Duration::from_secs(300);
        while parking_action(self.input(AgentActivity::Thinking)?) != ParkingAction::Park {
            while let Ok(event) = session.events.try_recv() {
                self.apply(&event);
            }
            if Instant::now() > deadline {
                return Err("The agent never called airc_wait".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let ended = self.runtimes.park(KEY).ok_or("No session to park")?;
        while let Some(event) = session.events.blocking_recv() {
            self.apply(&event);
        }
        let outcome = session.outcome.blocking_recv()?;
        self.runtimes
            .session_ended_with_shutdown(KEY, session.shutdown.confirmed());
        ended.blocking_recv()?;
        println!(
            "parked: session ended ({outcome:?}), processes stopped: {}, wait still recorded: {:?}",
            session.shutdown.confirmed(),
            self.channel.wait_state(MEMBER)?
        );
        Ok(())
    }

    fn wake(&mut self, load: bool) -> Result<(), Failure> {
        let action = parking_action(self.input(AgentActivity::Parked)?);
        println!("after the holder's PR closed: {action:?}");
        let ended = self.channel.wake_parked(MEMBER)?;
        let prompt = resume_prompt(ended.as_ref());
        println!("resume prompt: {prompt}");
        let blocks = self
            .runtimes
            .view(KEY)
            .map(|view| view.blocks)
            .unwrap_or_default();
        let earlier = self.session_id.clone().filter(|_| load);
        let resume = SessionResume::new(earlier, transcript_context(&blocks));
        let path = self.worktree.display().to_string();
        let inputs = self.runtimes.resume_parked(KEY, &path, &prompt, false)?;
        let mut session = spawn_session(self.agent.clone(), self.setup(Some(resume)), inputs);
        let mut turns = 0;
        while turns < 2 {
            let event = session
                .events
                .blocking_recv()
                .ok_or("The session ended early")?;
            turns += usize::from(matches!(event, AgentEvent::TurnEnded(_)));
            self.apply(&event);
        }
        for block in self
            .runtimes
            .view(KEY)
            .map(|view| view.blocks.to_vec())
            .unwrap_or_default()
        {
            match block {
                TranscriptBlock::UserMessage { text, .. } => println!("[message] {text}"),
                TranscriptBlock::AgentMessage { paragraphs } => {
                    println!("[agent] {}", paragraphs.join(" "));
                }
                _ => {}
            }
        }
        self.runtimes.stop(KEY);
        while let Some(event) = session.events.blocking_recv() {
            self.apply(&event);
        }
        Ok(())
    }
}
