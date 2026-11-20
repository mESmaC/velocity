//! Own queued commands alongside the ACP session, independently of model turns.

use std::future::pending;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use agent_client_protocol::v1::schema::{
    ContentBlock, EnvVariable, McpServer, McpServerStdio, SessionUpdate,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use velocity_core::queued_runs::{QueuedRunRecord, lost_runs_prompt};

use super::queued_run_command::{QueuedRun, RunResult, resolve_directory};
use super::queued_run_tool::{
    self, Accepted, REQUEST_BYTES, REQUEST_TIMEOUT, Request, RunCommand, Work,
};
use super::workflow_run_tool::WorkflowRequest;
use super::workflow_runs::WorkflowRun;
use super::{AgentEvent, AgentLaunch, SessionSetup, Shared};

static EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();

/// Registers the app executable, which serves the stdio side of the tool.
pub fn enable_queued_runs(executable: PathBuf) {
    let _ = EXECUTABLE.set(executable);
}

pub(super) struct RunQueue {
    stop: CancellationToken,
    worker: JoinHandle<()>,
}

impl RunQueue {
    pub async fn start(
        setup: &mut SessionSetup,
        launch: &AgentLaunch,
        restored: Vec<QueuedRunRecord>,
    ) -> io::Result<Option<(Self, RunResults)>> {
        let Some(executable) = EXECUTABLE.get() else {
            return Ok(None);
        };
        let Some(rosen) = velocity_rosen::mcp_command(&setup.worktree, &launch.env) else {
            return Ok(None);
        };
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let token = uuid::Uuid::new_v4().to_string();
        let port = listener.local_addr()?.port();
        attach_tool(setup, executable, port, &token);
        let (events, receiver) = unbounded_channel();
        let (released, turn_ended) = watch::channel(());
        let stop = CancellationToken::new();
        let outstanding = Arc::new(AtomicUsize::new(0));
        let host = QueueHost {
            token,
            rosen: rosen.program,
            worktree: setup.worktree.clone(),
            launch: launch.clone(),
            stop: stop.clone(),
            events,
            outstanding: Arc::clone(&outstanding),
            turn_ended,
        };
        let mut initial_runs = JoinSet::new();
        let mut results = RunResults {
            outstanding,
            receiver: Some(receiver),
            released: Some(released),
            ..RunResults::default()
        };
        host.start_restored(restored, &mut initial_runs, &mut results);
        let worker = tokio::spawn(host.serve(listener, initial_runs));
        Ok(Some((Self { stop, worker }, results)))
    }

    pub async fn stop(&mut self) -> io::Result<()> {
        self.stop.cancel();
        (&mut self.worker).await.map_err(io::Error::other)
    }
}

fn attach_tool(setup: &mut SessionSetup, executable: &Path, port: u16, token: &str) {
    use queued_run_tool::{QUEUED_RUN_PORT_ENV, QUEUED_RUN_TOKEN_ENV};

    // An app-owned tool can request the same background queue by declaring an empty port.
    // Give the capability only to servers that run this app's executable.
    let port = port.to_string();
    for server in &mut setup.shared_servers {
        if let McpServer::Stdio(server) = server
            && server.command == executable
            && let Some(variable) = server
                .env
                .iter_mut()
                .find(|variable| variable.name == QUEUED_RUN_PORT_ENV && variable.value.is_empty())
        {
            variable.value.clone_from(&port);
            server
                .env
                .push(EnvVariable::new(QUEUED_RUN_TOKEN_ENV, token));
        }
    }
    let server = McpServerStdio::new(queued_run_tool::QUEUED_RUN_SERVER, executable)
        .args(vec![queued_run_tool::QUEUED_RUN_SWITCH.into(), port])
        .env(vec![EnvVariable::new(QUEUED_RUN_TOKEN_ENV, token)]);
    setup.shared_servers.insert(0, McpServer::Stdio(server));
    let instructions = format!("[Velocity] {}", queued_run_tool::INSTRUCTIONS);
    setup.preface = Some(match setup.preface.take() {
        Some(preface) => format!("{preface}\n\n{instructions}"),
        None => instructions,
    });
}

impl Drop for RunQueue {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub(super) enum QueueEvent {
    Added(QueuedRunRecord),
    Started(Box<SessionUpdate>),
    Finished(RunResult),
    Cleared(String),
}

#[derive(Default)]
pub(super) struct RunResults {
    outstanding: Arc<AtomicUsize>,
    receiver: Option<UnboundedReceiver<QueueEvent>>,
    released: Option<watch::Sender<()>>,
    pub waiting: bool,
    pub restored_notice: bool,
    pub ready: Vec<ContentBlock>,
}

impl RunResults {
    pub async fn receive(&mut self) -> Option<QueueEvent> {
        match &mut self.receiver {
            Some(receiver) => receiver.recv().await,
            None => pending().await,
        }
    }

    /// Processes already-queued host events before sending an immediate startup prompt.
    pub fn drain_pending(&mut self, shared: &Shared) {
        let mut pending_events = Vec::new();
        if let Some(receiver) = &mut self.receiver {
            while let Ok(event) = receiver.try_recv() {
                pending_events.push(event);
            }
        }
        for event in pending_events {
            self.record(Some(event), shared);
        }
    }

    pub fn record_lost_runs(
        &mut self,
        runs: &[QueuedRunRecord],
        reason: &str,
        events: &UnboundedSender<AgentEvent>,
    ) {
        for run in runs {
            let _ = events.send(AgentEvent::QueuedRunFinished { id: run.id.clone() });
        }
        self.ready
            .push(ContentBlock::from(lost_runs_prompt(runs, Some(reason))));
        self.waiting = true;
    }

    pub fn record(&mut self, event: Option<QueueEvent>, shared: &Shared) {
        match event {
            Some(QueueEvent::Added(record)) => {
                shared.emit(AgentEvent::QueuedRunAdded(record));
            }
            Some(QueueEvent::Cleared(id)) => {
                shared.emit(AgentEvent::QueuedRunFinished { id });
            }
            Some(QueueEvent::Started(update)) => shared.record(*update),
            Some(QueueEvent::Finished(result)) => {
                if let Some(id) = result.persisted_id() {
                    shared.emit(AgentEvent::QueuedRunFinished { id: id.to_owned() });
                }
                self.outstanding.fetch_sub(1, Ordering::AcqRel);
                let (update, message) = result.into_update();
                shared.record(update);
                self.ready.push(message);
            }
            None => {
                self.receiver = None;
                if self.outstanding.swap(0, Ordering::AcqRel) != 0 {
                    self.ready.push(ContentBlock::from(
                        "[Velocity] The queued run stopped before reporting a result.",
                    ));
                }
            }
        }
    }

    pub fn outstanding(&self) -> bool {
        self.outstanding.load(Ordering::Acquire) != 0
    }

    pub fn release(&self) {
        if let Some(released) = &self.released {
            let _ = released.send(());
        }
    }
}

struct QueueHost {
    token: String,
    rosen: PathBuf,
    worktree: PathBuf,
    launch: AgentLaunch,
    stop: CancellationToken,
    events: UnboundedSender<QueueEvent>,
    outstanding: Arc<AtomicUsize>,
    turn_ended: watch::Receiver<()>,
}

impl QueueHost {
    fn start_restored(
        &self,
        restored: Vec<QueuedRunRecord>,
        runs: &mut JoinSet<RunResult>,
        results: &mut RunResults,
    ) {
        if restored.is_empty() {
            return;
        }
        let mut lost = Vec::new();
        let mut reasons = Vec::new();
        for record in restored {
            match self.spawn_restored(&record, runs) {
                Ok(()) => {}
                Err(reason) => {
                    let _ = self.events.send(QueueEvent::Cleared(record.id.clone()));
                    lost.push(record);
                    if !reasons.contains(&reason) {
                        reasons.push(reason);
                    }
                }
            }
        }
        if !lost.is_empty() {
            let joined = reasons.join("; ");
            results
                .ready
                .push(ContentBlock::from(lost_runs_prompt(&lost, Some(&joined))));
        }
        if results.outstanding() {
            results.waiting = true;
            results.restored_notice = true;
        } else if !results.ready.is_empty() {
            results.waiting = true;
        }
    }

    fn spawn_restored(
        &self,
        record: &QueuedRunRecord,
        runs: &mut JoinSet<RunResult>,
    ) -> Result<(), String> {
        let policy = velocity_rosen::agent_run::policy(&record.program, &record.arguments)?;
        let directory =
            resolve_directory(&self.worktree, record.directory.as_deref().map(Path::new));
        if !directory.is_dir() {
            return Err(format!(
                "working directory does not exist: {}",
                directory.display()
            ));
        }
        let run = QueuedRun::restored(record).map_err(|error| error.to_string())?;
        self.outstanding.fetch_add(1, Ordering::Release);
        let _ = self
            .events
            .send(QueueEvent::Started(Box::new(run.started())));
        let (rosen, worktree, launch, stop) = (
            self.rosen.clone(),
            self.worktree.clone(),
            self.launch.clone(),
            self.stop.clone(),
        );
        runs.spawn(async move { run.finish(&rosen, policy, &worktree, &launch, stop).await });
        Ok(())
    }

    async fn serve(self, listener: TcpListener, mut runs: JoinSet<RunResult>) {
        loop {
            tokio::select! {
                () = self.stop.cancelled() => break,
                connection = listener.accept() => match connection {
                    Ok((socket, _)) => self.accept(socket, &mut runs).await,
                    Err(_) => break,
                },
                Some(result) = runs.join_next() => {
                    let Ok(result) = result else { break };
                    let _ = self.events.send(QueueEvent::Finished(result));
                }
            }
        }
        self.stop.cancel();
        while runs.join_next().await.is_some() {}
    }

    async fn accept(&self, mut socket: TcpStream, runs: &mut JoinSet<RunResult>) {
        let reply = match tokio::time::timeout(REQUEST_TIMEOUT, read_request(&mut socket)).await {
            Ok(Ok(request)) if request.token == self.token => match request.work {
                Work::Command(command) => self.enqueue(command, runs).map(Accepted::Log),
                Work::Workflow(workflow) => self.start_workflow(workflow, runs).await,
            },
            Ok(Ok(_)) => Err("Invalid session token".into()),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("Timed out reading queued run request".into()),
        };
        if let Ok(mut line) = serde_json::to_vec(&reply) {
            line.push(b'\n');
            let _ = tokio::time::timeout(REQUEST_TIMEOUT, socket.write_all(&line)).await;
        }
    }

    /// Starts an owner-allowed GitHub workflow at once: it runs on GitHub, so unlike a
    /// queued command it has no reason to wait for the agent to leave its workspace.
    async fn start_workflow(
        &self,
        request: WorkflowRequest,
        runs: &mut JoinSet<RunResult>,
    ) -> Result<Accepted, String> {
        let run = WorkflowRun::admit(request, &self.launch).await?;
        let title = run.title.clone();
        self.outstanding.fetch_add(1, Ordering::Release);
        let _ = self
            .events
            .send(QueueEvent::Started(Box::new(run.started())));
        runs.spawn(run.finish(self.stop.clone()));
        Ok(Accepted::Workflow(title))
    }

    fn enqueue(
        &self,
        command: RunCommand,
        runs: &mut JoinSet<RunResult>,
    ) -> Result<PathBuf, String> {
        let policy = velocity_rosen::agent_run::policy(&command.program, &command.arguments)?;
        let run = QueuedRun::new(command).map_err(|error| error.to_string())?;
        let log = run.log.clone();
        let record = run.record();
        // Subscribe before acknowledging. A fast turn end must release this command
        // without allowing it to run during the agent's final handoff call.
        let mut turn_ended = self.turn_ended.clone();
        turn_ended.mark_unchanged();
        self.outstanding.fetch_add(1, Ordering::Release);
        let _ = self.events.send(QueueEvent::Added(record));
        let _ = self
            .events
            .send(QueueEvent::Started(Box::new(run.started())));
        let (rosen, worktree, launch, stop) = (
            self.rosen.clone(),
            self.worktree.clone(),
            self.launch.clone(),
            self.stop.clone(),
        );
        runs.spawn(async move {
            tokio::select! {
                released = turn_ended.changed() => {
                    if released.is_err() {
                        stop.cancel();
                    }
                }
                () = stop.cancelled() => {}
            }
            run.finish(&rosen, policy, &worktree, &launch, stop).await
        });
        Ok(log)
    }
}

async fn read_request(socket: &mut TcpStream) -> io::Result<Request> {
    let mut line = Vec::new();
    BufReader::new(socket)
        .take(REQUEST_BYTES)
        .read_until(b'\n', &mut line)
        .await?;
    if !line.ends_with(b"\n") {
        return Err(io::Error::other("Missing or oversized queued run request"));
    }
    Ok(serde_json::from_slice(&line)?)
}
