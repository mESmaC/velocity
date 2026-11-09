//! The interview's async edge (F3): runs the spec agent in the project's main checkout,
//! turns its replies into saved interview steps, and carries out your answers, Keep
//! grilling and Lock. Every decision is made by `turns`, `envelope` and `spec_file`.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use agent_client_protocol::v1::schema::{ContentBlock, TextContent};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use velocity_acp::Provider;
use velocity_acp::client::{AgentEvent, AgentInput, SessionSetup, run_session};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::{InterviewRecord, ProjectItem, ProviderKind, StateStore, ThreadState};

use crate::envelope::parse_envelope;
use crate::live::LiveTranscript;
use crate::turns::{self, InterviewDefaults};
use crate::{InterviewError, prompts};

/// A project and one of its threads.
pub type InterviewKey = (u32, u32);

/// What the interview needs from the app: the shared state, a way to publish the main
/// window, and a runtime to run agents on.
pub trait Host: Send + Sync + 'static {
    fn store(&self) -> &Mutex<StateStore>;
    fn layout(&self) -> &Mutex<LayoutState>;
    fn interviews(&self) -> &Interviews;
    fn publish(&self, snapshot: LayoutSnapshot);
    fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send>>);
}

/// Running interview agents and their live transcripts.
#[derive(Default)]
pub struct Interviews {
    sessions: Mutex<BTreeMap<InterviewKey, Session>>,
}

#[derive(Default)]
struct Session {
    /// `None` while no agent runs: before the first message, or after it stopped.
    inputs: Option<UnboundedSender<AgentInput>>,
    live: LiveTranscript,
    /// How often the agent said "ready" before SPEC.md existed and was asked to write it.
    spec_reminders: u8,
}

/// Reminders before the Ready card shows anyway, with a notice that SPEC.md is missing.
const MAX_SPEC_REMINDERS: u8 = 2;

fn lock<Value>(mutex: &Mutex<Value>) -> MutexGuard<'_, Value> {
    // A panic elsewhere must not stop the interview; the data is still consistent
    // because every store change is applied to a copy first.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Interviews {
    fn with<Value>(
        &self,
        key: InterviewKey,
        use_session: impl FnOnce(&mut Session) -> Value,
    ) -> Value {
        use_session(lock(&self.sessions).entry(key).or_default())
    }

    /// Whether the spec thread's agent is running, so deleting it stops one (F12).
    pub fn is_running(&self, key: InterviewKey) -> bool {
        lock(&self.sessions)
            .get(&key)
            .is_some_and(|session| session.inputs.is_some())
    }

    /// Ends the spec thread's agent because the thread is being deleted (F12). A spec
    /// thread works in the main checkout, so there is no worktree to wait for.
    pub fn stop_agent(&self, key: InterviewKey) {
        if let Some(inputs) = lock(&self.sessions)
            .get_mut(&key)
            .and_then(|session| session.inputs.take())
        {
            let _ = inputs.send(AgentInput::Stop);
        }
    }

    /// The live blocks and busy flag of every interview in `project`.
    fn live_for(&self, project: u32) -> Vec<(u32, Vec<TranscriptBlock>, bool)> {
        lock(&self.sessions)
            .iter()
            .filter(|((owner, _), _)| *owner == project)
            .map(|((_, thread), session)| (*thread, session.live.blocks(), session.live.busy()))
            .collect()
    }
}

/// Lays the live transcripts over a freshly rebuilt main window. Call with the store and
/// layout already locked, after anything that rebuilds the layout from the store.
pub fn overlay_live(interviews: &Interviews, store: &StateStore, layout: &mut LayoutState) {
    let Some(project) = store.active_project_identifier() else {
        return;
    };
    for (thread, blocks, busy) in interviews.live_for(project) {
        layout.set_thread_live(thread as usize, blocks, busy);
    }
}

/// Rebuilds the main window from the store, lays the live transcripts over it, publishes.
pub fn refresh<H: Host>(host: &H) {
    let store = lock(host.store());
    let mut layout = lock(host.layout());
    store.refresh_layout(&mut layout);
    overlay_live(host.interviews(), &store, &mut layout);
    let snapshot = layout.snapshot();
    drop(layout);
    drop(store);
    host.publish(snapshot);
}

/// Applies `change` to the project and saves it, then republishes. A failed save keeps
/// the old state; it is reported on stderr because no view is waiting for it.
fn save<H: Host, Value>(
    host: &H,
    project: u32,
    change: impl FnOnce(&mut ProjectItem) -> Value,
) -> Option<Value> {
    let saved = lock(host.store()).update_project(project, |item| {
        Ok::<_, velocity_store::projects::ProjectError>(change(item))
    });
    refresh(host);
    match saved {
        Ok(value) => Some(value),
        Err(error) => {
            eprintln!("Could not save the spec interview: {error}");
            None
        }
    }
}

/// Starts every interview Create recorded, and restarts interviews a restart cut off
/// while they were scanning. Called at launch and after Create (F2: "starts the spec
/// interview thread straight away").
pub fn start_interviews<H: Host>(host: &Arc<H>) {
    for (key, prompt) in interviews_to_start(host.as_ref()) {
        launch(host, key, prompt);
    }
    refresh(host.as_ref());
    open_active_interview(host.as_ref());
}

/// Opens the project's saved thread when nothing is open: the spec thread a pending
/// interview just became did not exist yet when the window restored the project (F2:
/// Create "switches to it and starts the spec interview").
fn open_active_interview<H: Host>(host: &H) {
    let store = lock(host.store());
    let Some(thread) = crate::project_layout_thread(&store) else {
        return;
    };
    let mut layout = lock(host.layout());
    if layout.selected_thread().is_some() {
        return;
    }
    layout.apply(velocity_core::layout_view::LayoutAction::OpenThread(thread));
    overlay_live(host.interviews(), &store, &mut layout);
    let snapshot = layout.snapshot();
    drop(layout);
    drop(store);
    host.publish(snapshot);
}

fn interviews_to_start<H: Host>(host: &H) -> Vec<(InterviewKey, String)> {
    let mut store = lock(host.store());
    let settings = store.settings();
    let defaults = InterviewDefaults {
        provider: settings.providers.default_provider,
        model_name: settings.providers.default_model.unwrap_or_default(),
    };
    let pending: Vec<u32> = store
        .state()
        .projects
        .iter()
        .filter(|project| project.pending_interview.is_some())
        .map(|project| project.id)
        .collect();
    for project in pending {
        let started = store.update_project(project, |item| {
            Ok::<_, velocity_store::projects::ProjectError>(turns::start_from_pending(
                item, &defaults,
            ))
        });
        if let Err(error) = started {
            eprintln!("Could not start the spec interview: {error}");
        }
    }
    let running = lock(&host.interviews().sessions);
    store
        .state()
        .projects
        .iter()
        .flat_map(|project| {
            scanning_threads(project).map(move |(thread, prompt)| ((project.id, thread), prompt))
        })
        .filter(|(key, _)| {
            running
                .get(key)
                .is_none_or(|session| session.inputs.is_none())
        })
        .collect()
}

/// Interviews still in the scan with no agent: new ones, or ones a restart cut off.
fn scanning_threads(project: &ProjectItem) -> impl Iterator<Item = (u32, String)> + '_ {
    project.threads.iter().filter_map(move |thread| {
        let record = thread.interview.as_ref()?;
        let scanning = record.stage == velocity_store::InterviewStage::Scan
            && thread.state == ThreadState::Spec;
        scanning.then(|| (thread.id, opening_or_resume(project, record)))
    })
}

fn opening_or_resume(project: &ProjectItem, record: &InterviewRecord) -> String {
    if record.decisions.is_empty() {
        prompts::opening_prompt(&project.name, &record.brief, &project.spec_path)
    } else {
        prompts::resume_prompt(&project.name, &project.spec_path, record)
    }
}

/// What an agent needs to start: where, and which provider's command.
struct LaunchPlan {
    checkout: PathBuf,
    provider: ProviderKind,
    brief: String,
}

fn launch_plan<H: Host>(host: &H, (project, thread): InterviewKey) -> Option<LaunchPlan> {
    let store = lock(host.store());
    let item = store
        .state()
        .projects
        .iter()
        .find(|item| item.id == project)?;
    let spec_thread = item.threads.iter().find(|item| item.id == thread)?;
    Some(LaunchPlan {
        checkout: PathBuf::from(&item.repository_path),
        provider: spec_thread.provider,
        brief: spec_thread.interview.as_ref()?.brief.clone(),
    })
}

/// Starts the interview's agent in the main checkout (F3: no worktree) and sends it
/// `prompt`.
fn launch<H: Host>(host: &Arc<H>, key: InterviewKey, prompt: String) {
    let Some(plan) = launch_plan(host.as_ref(), key) else {
        return;
    };
    let Some(agent) = velocity_acp::provider_agent(acp_provider(plan.provider)) else {
        let message = InterviewError::NoAgent(plan.provider).to_string();
        save(host.as_ref(), key.0, |project| {
            turns::fail(project, key.1, message)
        });
        return;
    };
    let (inputs, receiver) = unbounded_channel();
    // Queued until the session has started: the client reads inputs only after `session/new`.
    let _ = inputs.send(AgentInput::Send(text_message(prompt)));
    host.interviews().with(key, |session| {
        if session.live.blocks().is_empty() && !plan.brief.trim().is_empty() {
            session.live.push(TranscriptBlock::UserMessage {
                text: plan.brief.clone(),
                queued: false,
            });
        }
        session.live.begin_turn();
        session.inputs = Some(inputs);
    });
    let setup = SessionSetup {
        worktree: plan.checkout,
        airc: None,
        shared_servers: Vec::new(),
        bypass_modes: agent.bypass_modes,
    };
    let task_host = Arc::clone(host);
    host.spawn(Box::pin(async move {
        run(task_host, key, agent.launch, setup, receiver).await;
    }));
}

async fn run<H: Host>(
    host: Arc<H>,
    key: InterviewKey,
    launch: velocity_acp::client::AgentLaunch,
    setup: SessionSetup,
    inputs: UnboundedReceiver<AgentInput>,
) {
    let (events, mut received) = unbounded_channel();
    let mut agent_log = Vec::new();
    let session = run_session(launch, setup, inputs, events);
    tokio::pin!(session);
    // The session's end decides when the interview stops, not the event stream: on Windows
    // an agent behind a `.cmd` shim can outlive the shim and keep its stderr open.
    let outcome = loop {
        tokio::select! {
            outcome = &mut session => break outcome,
            Some(event) = received.recv() => handle_event(host.as_ref(), key, event, &mut agent_log),
        }
    };
    while let Ok(event) = received.try_recv() {
        handle_event(host.as_ref(), key, event, &mut agent_log);
    }
    host.interviews().with(key, |session| {
        session.inputs = None;
        session.live.end_turn();
    });
    match outcome {
        Ok(()) => refresh(host.as_ref()),
        Err(error) => {
            let message = failure_message(&error.to_string(), &agent_log);
            save(host.as_ref(), key.0, |project| {
                turns::fail(project, key.1, message)
            });
        }
    }
}

/// How many of the agent's first stderr lines a failure shows: ACP errors alone often say
/// only "Internal error", while the agent's log says why, usually first (a refused start,
/// a missing login).
const LOG_LINES: usize = 6;

fn failure_message(error: &str, agent_log: &[String]) -> String {
    let mut message = format!("The spec agent stopped: {error}");
    if !agent_log.is_empty() {
        message.push('\n');
        message.push_str(&agent_log.join("\n"));
    }
    message
}

/// Feeds one agent event into the live transcript and the saved interview.
fn handle_event<H: Host>(
    host: &H,
    key: InterviewKey,
    event: AgentEvent,
    agent_log: &mut Vec<String>,
) {
    match event {
        AgentEvent::Transcript { placement, update } => {
            host.interviews()
                .with(key, |session| session.live.record(placement, &update));
            publish_live(host, key);
        }
        AgentEvent::TurnEnded(outcome) => turn_ended(host, key, outcome),
        AgentEvent::Log(line) if !line.trim().is_empty() && agent_log.len() < LOG_LINES => {
            agent_log.push(line);
        }
        _ => {}
    }
}

fn turn_ended<H: Host>(
    host: &H,
    key: InterviewKey,
    outcome: Result<agent_client_protocol::v1::schema::StopReason, String>,
) {
    let reply = host
        .interviews()
        .with(key, |session| session.live.end_turn());
    match outcome {
        Ok(_) => {
            let envelope = parse_envelope(&reply);
            if envelope.as_ref().is_some_and(|reply| reply.ready.is_some())
                && remind_to_write_spec(host, key)
            {
                let tasks = envelope.map(|reply| reply.tasks).unwrap_or_default();
                save(host, key.0, |project| {
                    turns::add_draft_tasks(project, tasks)
                });
                return;
            }
            save(host, key.0, |project| {
                turns::apply_reply(project, key.1, envelope)
            });
        }
        Err(error) => {
            let message = format!("The spec agent's turn failed: {error}");
            save(host, key.0, |project| turns::fail(project, key.1, message));
        }
    }
}

/// "Ready" means SPEC.md is written (F3: Read SPEC.md, Lock commits it). When the agent
/// says ready without it, it is asked to write the file first; returns whether it was.
fn remind_to_write_spec<H: Host>(host: &H, key: InterviewKey) -> bool {
    let Some((file, spec_path)) = spec_location(host, key) else {
        return false;
    };
    if file.is_file() {
        return false;
    }
    host.interviews().with(key, |session| {
        let Some(inputs) = session.inputs.as_ref() else {
            return false;
        };
        if session.spec_reminders >= MAX_SPEC_REMINDERS {
            return false;
        }
        let prompt = prompts::write_spec_prompt(&spec_path);
        if inputs.send(AgentInput::Send(text_message(prompt))).is_err() {
            return false;
        }
        session.spec_reminders += 1;
        session.live.begin_turn();
        true
    })
}

fn spec_location<H: Host>(host: &H, (project, _): InterviewKey) -> Option<(PathBuf, String)> {
    let store = lock(host.store());
    let item = store
        .state()
        .projects
        .iter()
        .find(|item| item.id == project)?;
    Some((
        spec_file_path(&item.repository_path, &item.spec_path),
        item.spec_path.clone(),
    ))
}

/// Publishes the streamed transcript without rebuilding the rest of the window.
fn publish_live<H: Host>(host: &H, (project, thread): InterviewKey) {
    let store = lock(host.store());
    if store.active_project_identifier() != Some(project) {
        return;
    }
    let mut layout = lock(host.layout());
    let (blocks, busy) = host.interviews().with((project, thread), |session| {
        (session.live.blocks(), session.live.busy())
    });
    layout.set_thread_live(thread as usize, blocks, busy);
    let snapshot = layout.snapshot();
    drop(layout);
    drop(store);
    host.publish(snapshot);
}

/// Sends `prompt` to the interview's agent, starting it again when none runs (after a
/// restart or an error). `shown` is what the transcript shows for it.
pub(crate) fn send<H: Host>(
    host: &Arc<H>,
    key: InterviewKey,
    prompt: String,
    shown: TranscriptBlock,
) {
    save(host.as_ref(), key.0, |project| {
        turns::mark_working(project, key.1)
    });
    let delivered = host.interviews().with(key, |session| {
        session.live.push(shown);
        let sent = session.inputs.as_ref().is_some_and(|inputs| {
            inputs
                .send(AgentInput::Send(text_message(prompt.clone())))
                .is_ok()
        });
        if sent {
            session.live.begin_turn();
        }
        sent
    });
    if !delivered {
        let resume = resume_with(host.as_ref(), key, &prompt);
        launch(host, key, resume);
    }
    refresh(host.as_ref());
}

fn resume_with<H: Host>(host: &H, (project, thread): InterviewKey, prompt: &str) -> String {
    let store = lock(host.store());
    let context = store
        .state()
        .projects
        .iter()
        .find(|item| item.id == project)
        .and_then(|item| {
            let record = item
                .threads
                .iter()
                .find(|item| item.id == thread)?
                .interview
                .as_ref()?;
            Some(prompts::resume_prompt(&item.name, &item.spec_path, record))
        })
        .unwrap_or_default();
    format!("{context}\n\n{prompt}")
}

/// Ends the interview's agent (after Lock).
pub(crate) fn stop<H: Host>(host: &H, key: InterviewKey) {
    host.interviews().with(key, |session| {
        if let Some(inputs) = session.inputs.take() {
            let _ = inputs.send(AgentInput::Stop);
        }
    });
}

pub(crate) fn save_step<H: Host, Value>(
    host: &H,
    project: u32,
    change: impl FnOnce(&mut ProjectItem) -> Value,
) -> Option<Value> {
    save(host, project, change)
}

pub(crate) fn locked_store<H: Host>(host: &H) -> MutexGuard<'_, StateStore> {
    lock(host.store())
}

pub(crate) fn locked_layout<H: Host>(host: &H) -> MutexGuard<'_, LayoutState> {
    lock(host.layout())
}

fn text_message(text: String) -> Vec<ContentBlock> {
    vec![ContentBlock::Text(TextContent::new(text))]
}

fn acp_provider(provider: ProviderKind) -> Provider {
    match provider {
        ProviderKind::Claude => Provider::Anthropic,
        ProviderKind::Codex => Provider::Codex,
        ProviderKind::Antigravity => Provider::Antigravity,
        ProviderKind::Kimi => Provider::KimiCode,
    }
}

/// The spec file inside the project's checkout.
pub(crate) fn spec_file_path(checkout: &str, spec_path: &str) -> PathBuf {
    Path::new(checkout).join(spec_path)
}
