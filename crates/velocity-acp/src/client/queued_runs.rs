//! Own queued commands alongside the ACP session, independently of model turns.

use std::future::pending;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use agent_client_protocol::v1::schema::{
    ContentBlock, EnvVariable, McpServer, McpServerStdio, SessionUpdate, StopReason,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use velocity_core::queued_runs::{
    MAX_RUN_HISTORY, QueuedRunPhase, QueuedRunRecord, continue_after_restart_prompt,
    lost_runs_prompt, lost_runs_prompt_with_results, restarted_runs_notice,
};
use velocity_rosen::agent_run::{AgentRun, BLOCK_VCS_ENV};
use velocity_rosen::queued_turn::{QUEUE_ENV, QueueEstimate, estimate};

use super::preflight_run::PreflightRun;
use super::queued_run_command::{QueuedRun, RunResult, read_log_excerpt, resolve_directory};
use super::queued_run_tool::{
    self, Accepted, QueuedRunAccepted, REQUEST_BYTES, REQUEST_TIMEOUT, Request, RunCancelRequest,
    RunCommand, RunStatusRequest, Work,
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
        history: Vec<QueuedRunRecord>,
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
        let registry = Arc::new(Mutex::new(RunRegistry::from_history(&history)));
        let graph_context = setup.airc.as_ref().and_then(super::airc::airc_project).map(
            |(airc, app_data, project)| {
                (
                    airc.command.clone(),
                    PathBuf::from(app_data),
                    project.to_owned(),
                )
            },
        );
        let host = QueueHost {
            token,
            rosen: rosen.program,
            worktree: setup.worktree.clone(),
            launch: launch.clone(),
            graph_context,
            stop: stop.clone(),
            events,
            outstanding: Arc::clone(&outstanding),
            turn_ended,
            registry: Arc::clone(&registry),
        };
        let mut initial_runs = JoinSet::new();
        let mut results = RunResults {
            outstanding,
            receiver: Some(receiver),
            released: Some(released),
            registry: Some(registry),
            ..RunResults::default()
        };
        host.start_restored(restored, history, &mut initial_runs, &mut results);
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
    let mut arguments = vec![
        queued_run_tool::QUEUED_RUN_SWITCH.into(),
        port,
        "--role".into(),
        setup.role.as_str().into(),
    ];
    if setup.lazy_tools {
        arguments.push("--lazy".into());
    }
    let server = McpServerStdio::new(queued_run_tool::QUEUED_RUN_SERVER, executable)
        .args(arguments)
        .env(vec![EnvVariable::new(QUEUED_RUN_TOKEN_ENV, token)]);
    setup.shared_servers.insert(0, McpServer::Stdio(server));
}

impl Drop for RunQueue {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub(super) enum QueueEvent {
    Added(QueuedRunRecord),
    Updated(QueuedRunRecord),
    Started(Box<SessionUpdate>),
    Finished(RunResult),
    Cleared(QueuedRunRecord),
}

pub(super) struct RestartNotice {
    pub interrupted: Vec<QueuedRunRecord>,
    pub pre_shutdown_results: Vec<QueuedRunRecord>,
}

#[derive(Default)]
pub(super) struct RunResults {
    outstanding: Arc<AtomicUsize>,
    receiver: Option<UnboundedReceiver<QueueEvent>>,
    released: Option<watch::Sender<()>>,
    registry: Option<Arc<Mutex<RunRegistry>>>,
    pub waiting: bool,
    pub restart_notice: Option<RestartNotice>,
    pub deferred_outcome: Option<Result<StopReason, String>>,
    delivered_ids: Vec<String>,
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
        pre_shutdown_results: &[QueuedRunRecord],
        reason: &str,
        events: &UnboundedSender<AgentEvent>,
    ) {
        for run in runs {
            let mut failed = run.clone();
            failed.phase = QueuedRunPhase::Failed;
            failed.queue_reason = None;
            failed.exit_status = Some(format!("could not restart: {reason}"));
            failed.delivered = true;
            let _ = events.send(AgentEvent::QueuedRunFinished(failed));
        }
        for result in pre_shutdown_results {
            self.push_delivered_id(&result.id);
        }
        if !runs.is_empty() {
            self.ready
                .push(ContentBlock::from(lost_runs_prompt_with_results(
                    runs,
                    pre_shutdown_results,
                    Some(reason),
                )));
            self.waiting = true;
        } else if !pre_shutdown_results.is_empty() {
            self.ready
                .push(ContentBlock::from(continue_after_restart_prompt(
                    &[],
                    pre_shutdown_results,
                )));
            self.waiting = true;
        }
    }

    pub fn record(&mut self, event: Option<QueueEvent>, shared: &Shared) {
        match event {
            Some(QueueEvent::Added(record)) => {
                shared.emit(AgentEvent::QueuedRunAdded(record));
            }
            Some(QueueEvent::Updated(record)) => {
                shared.emit(AgentEvent::QueuedRunUpdated(record));
            }
            Some(QueueEvent::Cleared(record)) => {
                shared.emit(AgentEvent::QueuedRunFinished(record));
            }
            Some(QueueEvent::Started(update)) => shared.record(*update),
            Some(QueueEvent::Finished(result)) => {
                if let Some(record) = result.record() {
                    if result.deliver_prompt() {
                        self.push_delivered_id(&record.id);
                    }
                    shared.emit(AgentEvent::QueuedRunFinished(record.clone()));
                }
                self.outstanding.fetch_sub(1, Ordering::AcqRel);
                let (update, message, deliver_prompt) = result.into_update();
                shared.record(update);
                if deliver_prompt {
                    self.ready.push(message);
                }
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

    pub fn take_restart_notice(&mut self) -> Option<ContentBlock> {
        let notice = self.restart_notice.take()?;
        Some(ContentBlock::from(restarted_runs_notice(
            &notice.interrupted,
            &notice.pre_shutdown_results,
        )))
    }

    pub fn take_delivered_ids(&mut self) -> Vec<String> {
        let ids = std::mem::take(&mut self.delivered_ids);
        if !ids.is_empty()
            && let Some(registry) = &self.registry
        {
            registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .mark_delivered(&ids);
        }
        ids
    }

    pub fn outstanding(&self) -> bool {
        self.outstanding.load(Ordering::Acquire) != 0
    }

    pub fn release(&self) {
        if let Some(released) = &self.released {
            let _ = released.send(());
        }
    }

    fn push_delivered_id(&mut self, id: &str) {
        if !self.delivered_ids.iter().any(|existing| existing == id) {
            self.delivered_ids.push(id.to_owned());
        }
    }
}

struct TrackedRun {
    record: QueuedRunRecord,
    policy: Option<AgentRun>,
    cancel: Option<CancellationToken>,
    cancelled_by_user: Arc<AtomicBool>,
}

pub(super) struct RunRegistry {
    runs: Vec<TrackedRun>,
}

impl RunRegistry {
    fn from_history(history: &[QueuedRunRecord]) -> Self {
        let runs = history
            .iter()
            .cloned()
            .map(|record| TrackedRun {
                record,
                policy: None,
                cancel: None,
                cancelled_by_user: Arc::new(AtomicBool::new(false)),
            })
            .collect();
        Self { runs }
    }

    fn register_active(
        &mut self,
        record: QueuedRunRecord,
        policy: AgentRun,
        cancel: CancellationToken,
    ) -> Arc<AtomicBool> {
        let cancelled_by_user = Arc::new(AtomicBool::new(false));
        if let Some(existing) = self.runs.iter_mut().find(|run| run.record.id == record.id) {
            existing.record = record;
            existing.policy = Some(policy);
            existing.cancel = Some(cancel);
            existing.cancelled_by_user = Arc::clone(&cancelled_by_user);
        } else {
            self.runs.push(TrackedRun {
                record,
                policy: Some(policy),
                cancel: Some(cancel),
                cancelled_by_user: Arc::clone(&cancelled_by_user),
            });
        }
        cancelled_by_user
    }

    fn update_record(
        &mut self,
        run_id: &str,
        mutate: impl FnOnce(&mut QueuedRunRecord),
    ) -> Option<QueuedRunRecord> {
        let entry = self.runs.iter_mut().find(|run| run.record.id == run_id)?;
        mutate(&mut entry.record);
        Some(entry.record.clone())
    }

    fn finish(&mut self, record: QueuedRunRecord) {
        if let Some(existing) = self.runs.iter_mut().find(|run| run.record.id == record.id) {
            existing.record = record;
            existing.cancel = None;
        } else {
            self.runs.push(TrackedRun {
                record,
                policy: None,
                cancel: None,
                cancelled_by_user: Arc::new(AtomicBool::new(false)),
            });
        }
        self.trim_history();
    }

    fn mark_delivered(&mut self, ids: &[String]) {
        for run in &mut self.runs {
            if ids.iter().any(|id| id == &run.record.id) {
                run.record.delivered = true;
            }
        }
    }

    fn pending_count_before(&self, run_id: Option<&str>) -> usize {
        let mut count = 0;
        for run in &self.runs {
            if run_id.is_some_and(|id| id == run.record.id) {
                break;
            }
            if matches!(
                run.record.phase,
                QueuedRunPhase::WaitingForTurnEnd | QueuedRunPhase::Queued
            ) {
                count += 1;
            }
        }
        count
    }

    fn snapshot(&self, run_id: &str) -> Option<(QueuedRunRecord, Option<AgentRun>, usize)> {
        let pending_ahead = self.pending_count_before(Some(run_id));
        self.runs
            .iter()
            .find(|run| run.record.id == run_id)
            .map(|run| (run.record.clone(), run.policy, pending_ahead))
    }

    fn all_records(&self) -> Vec<QueuedRunRecord> {
        self.runs.iter().map(|run| run.record.clone()).collect()
    }

    fn cancel(&mut self, run_id: &str) -> Result<(QueuedRunRecord, bool), String> {
        let Some(entry) = self.runs.iter_mut().find(|run| run.record.id == run_id) else {
            return Err(format!("Unknown run ID: {run_id}"));
        };
        if entry.record.phase.is_terminal() {
            return Ok((entry.record.clone(), false));
        }
        entry.cancelled_by_user.store(true, Ordering::Release);
        if let Some(cancel) = entry.cancel.take() {
            cancel.cancel();
        }
        entry.record.phase = QueuedRunPhase::Cancelled;
        entry.record.queue_reason = None;
        entry.record.exit_status = Some("cancelled".into());
        entry.record.delivered = true;
        Ok((entry.record.clone(), true))
    }

    fn trim_history(&mut self) {
        let terminal = self
            .runs
            .iter()
            .filter(|run| run.record.phase.is_terminal())
            .count();
        let mut excess = terminal.saturating_sub(MAX_RUN_HISTORY);
        if excess == 0 {
            return;
        }
        self.runs.retain(|run| {
            if excess > 0 && run.record.phase.is_terminal() {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }
}

#[derive(Clone)]
pub(super) struct RunStatusSink {
    run_id: String,
    registry: Arc<Mutex<RunRegistry>>,
    events: UnboundedSender<QueueEvent>,
    cancelled_by_user: Arc<AtomicBool>,
}

impl RunStatusSink {
    pub(super) fn set_queued(&self, reason: Option<String>) {
        let updated = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .update_record(&self.run_id, |record| {
                if !record.phase.is_terminal() {
                    record.phase = QueuedRunPhase::Queued;
                    record.queue_reason = reason;
                }
            });
        if let Some(record) = updated {
            let _ = self.events.send(QueueEvent::Updated(record));
        }
    }

    pub(super) fn set_queue_reason(&self, reason: &str) {
        let updated = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .update_record(&self.run_id, |record| {
                if !record.phase.is_terminal() {
                    record.phase = QueuedRunPhase::Queued;
                    record.queue_reason = Some(reason.to_owned());
                }
            });
        if let Some(record) = updated {
            let _ = self.events.send(QueueEvent::Updated(record));
        }
    }

    pub(super) fn set_running(&self, change: Option<String>, head: Option<String>) {
        let updated = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .update_record(&self.run_id, |record| {
                if !record.phase.is_terminal() {
                    record.phase = QueuedRunPhase::Running;
                    record.queue_reason = None;
                    if change.is_some() {
                        record.change = change;
                    }
                    if head.is_some() {
                        record.head = head;
                    }
                }
            });
        if let Some(record) = updated {
            let _ = self.events.send(QueueEvent::Updated(record));
        }
    }

    pub(super) fn is_cancelled_by_user(&self) -> bool {
        self.cancelled_by_user.load(Ordering::Acquire)
    }

    pub(super) fn finish(&self, record: QueuedRunRecord) {
        self.registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .finish(record);
    }
}

struct QueueHost {
    token: String,
    rosen: PathBuf,
    worktree: PathBuf,
    launch: AgentLaunch,
    graph_context: Option<(PathBuf, PathBuf, String)>,
    stop: CancellationToken,
    events: UnboundedSender<QueueEvent>,
    outstanding: Arc<AtomicUsize>,
    turn_ended: watch::Receiver<()>,
    registry: Arc<Mutex<RunRegistry>>,
}

impl QueueHost {
    fn start_restored(
        &self,
        restored: Vec<QueuedRunRecord>,
        history: Vec<QueuedRunRecord>,
        runs: &mut JoinSet<RunResult>,
        results: &mut RunResults,
    ) {
        let pre_shutdown_results: Vec<QueuedRunRecord> = history
            .into_iter()
            .filter(|record| !record.delivered)
            .collect();
        for result in &pre_shutdown_results {
            results.push_delivered_id(&result.id);
        }
        if restored.is_empty() {
            if !pre_shutdown_results.is_empty() {
                results
                    .ready
                    .push(ContentBlock::from(continue_after_restart_prompt(
                        &[],
                        &pre_shutdown_results,
                    )));
                results.waiting = true;
            }
            return;
        }
        let mut restarted = Vec::new();
        let mut lost = Vec::new();
        let mut reasons = Vec::new();
        for record in restored {
            match self.spawn_restored(&record, runs) {
                Ok(()) => restarted.push(record),
                Err(reason) => {
                    self.record_lost_restored(&record, &reason);
                    lost.push(record);
                    if !reasons.contains(&reason) {
                        reasons.push(reason);
                    }
                }
            }
        }
        apply_restored_notices(results, restarted, lost, &reasons, pre_shutdown_results);
    }

    fn record_lost_restored(&self, record: &QueuedRunRecord, reason: &str) {
        let mut failed = record.clone();
        failed.phase = QueuedRunPhase::Failed;
        failed.queue_reason = None;
        failed.exit_status = Some(format!("could not restart: {reason}"));
        failed.delivered = true;
        self.registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .finish(failed.clone());
        let _ = self.events.send(QueueEvent::Cleared(failed));
    }

    fn spawn_restored(
        &self,
        record: &QueuedRunRecord,
        runs: &mut JoinSet<RunResult>,
    ) -> Result<(), String> {
        let policy = self.command_policy(&record.program, &record.arguments)?;
        let directory =
            resolve_directory(&self.worktree, record.directory.as_deref().map(Path::new));
        if !directory.is_dir() {
            return Err(format!(
                "working directory does not exist: {}",
                directory.display()
            ));
        }
        let run =
            QueuedRun::restored(record, &self.worktree, &self.launch).map_err(|e| e.to_string())?;
        let active_record = run.record();
        let run_stop = self.stop.child_token();
        let cancelled_by_user = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .register_active(active_record.clone(), policy, run_stop.clone());
        let sink = RunStatusSink {
            run_id: active_record.id.clone(),
            registry: Arc::clone(&self.registry),
            events: self.events.clone(),
            cancelled_by_user,
        };
        self.outstanding.fetch_add(1, Ordering::Release);
        let _ = self.events.send(QueueEvent::Updated(active_record));
        let _ = self
            .events
            .send(QueueEvent::Started(Box::new(run.started())));
        let (rosen, worktree, launch) = (
            self.rosen.clone(),
            self.worktree.clone(),
            self.launch.clone(),
        );
        runs.spawn(async move {
            run.finish(&rosen, policy, &worktree, &launch, run_stop, sink)
                .await
        });
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
                Work::Command(command) => self.enqueue(command, runs).map(Accepted::Queued),
                Work::Workflow(workflow) => self.start_workflow(workflow, runs).await,
                Work::Status(query) => self.status(query).await.map(Accepted::Json),
                Work::Cancel(query) => self.cancel(query).map(Accepted::Json),
                Work::Preflight(_) => self.start_preflight(runs).map(Accepted::Log),
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

    /// Runs the change's preflight checks after the agent ends its turn so no concurrent
    /// edits race with snapshotting, formatting or crate checks.
    fn start_preflight(&self, runs: &mut JoinSet<RunResult>) -> Result<PathBuf, String> {
        let run = PreflightRun::admit(&self.worktree, &self.launch, self.graph_context.clone())?;
        let log = run.log.clone();
        let mut turn_ended = self.turn_ended.clone();
        turn_ended.mark_unchanged();
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
        runs.spawn(async move {
            tokio::select! {
                released = turn_ended.changed() => {
                    if released.is_err() {
                        stop.cancel();
                    }
                }
                () = stop.cancelled() => {}
            }
            run.finish(&rosen, &worktree, &launch, stop).await
        });
        Ok(log)
    }

    fn enqueue(
        &self,
        command: RunCommand,
        runs: &mut JoinSet<RunResult>,
    ) -> Result<QueuedRunAccepted, String> {
        let policy = self.command_policy(&command.program, &command.arguments)?;
        let pending_ahead = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending_count_before(None);
        let estimated = self.estimate_queue(policy, command.run_class(), pending_ahead);
        let run = QueuedRun::new(command, &self.worktree, &self.launch)
            .map_err(|error| error.to_string())?;
        let record = run.record();
        let accepted = accepted_reply(&record, run.log.clone(), estimated);
        let (run_stop, sink) = self.register_run(record.clone(), policy);
        // Subscribe before acknowledging. A fast turn end must release this command
        // without allowing it to run during the agent's final handoff call.
        let mut turn_ended = self.turn_ended.clone();
        turn_ended.mark_unchanged();
        self.outstanding.fetch_add(1, Ordering::Release);
        let _ = self.events.send(QueueEvent::Added(record));
        let _ = self
            .events
            .send(QueueEvent::Started(Box::new(run.started())));
        let (rosen, worktree, launch) = (
            self.rosen.clone(),
            self.worktree.clone(),
            self.launch.clone(),
        );
        runs.spawn(async move {
            tokio::select! {
                released = turn_ended.changed() => {
                    if released.is_err() {
                        run_stop.cancel();
                    }
                }
                () = run_stop.cancelled() => {}
            }
            run.finish(&rosen, policy, &worktree, &launch, run_stop, sink)
                .await
        });
        Ok(accepted)
    }

    fn register_run(
        &self,
        record: QueuedRunRecord,
        policy: AgentRun,
    ) -> (CancellationToken, RunStatusSink) {
        let run_stop = self.stop.child_token();
        let cancelled_by_user = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .register_active(record.clone(), policy, run_stop.clone());
        let sink = RunStatusSink {
            run_id: record.id,
            registry: Arc::clone(&self.registry),
            events: self.events.clone(),
            cancelled_by_user,
        };
        (run_stop, sink)
    }

    fn estimate_queue(
        &self,
        policy: AgentRun,
        class: velocity_core::queued_runs::QueuedRunClass,
        pending_ahead: usize,
    ) -> QueueEstimate {
        let queue_override = self.launch.env_value(QUEUE_ENV).map(PathBuf::from);
        estimate(
            policy,
            class,
            &self.worktree,
            queue_override.as_deref(),
            pending_ahead,
        )
    }

    async fn status(&self, query: RunStatusRequest) -> Result<Value, String> {
        let Some(run_id) = query.run_id else {
            return Ok(self.list_runs_json());
        };
        let Some((record, policy, pending_ahead)) = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot(&run_id)
        else {
            return Err(format!("Unknown run ID: {run_id}"));
        };
        let queue_estimate = if matches!(
            record.phase,
            QueuedRunPhase::WaitingForTurnEnd | QueuedRunPhase::Queued
        ) {
            policy
                .or_else(|| self.command_policy(&record.program, &record.arguments).ok())
                .map(|policy| self.estimate_queue(policy, record.class, pending_ahead))
        } else {
            None
        };
        let log_excerpt = read_excerpt_async(record.log.as_deref()).await;
        Ok(format_run_status(
            &record,
            queue_estimate.as_ref(),
            log_excerpt,
        ))
    }

    fn list_runs_json(&self) -> Value {
        let records = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .all_records();
        let list: Vec<Value> = records
            .iter()
            .rev()
            .map(|record| {
                json!({
                    "run_id": record.id,
                    "command": record.command_line(),
                    "class": record.class.as_str(),
                    "phase": record.phase.as_str(),
                    "queue_reason": record.queue_reason,
                    "change": record.change,
                    "head": record.head,
                    "exit_code": record.exit_code,
                    "exit_status": record.exit_status,
                    "log": record.log,
                })
            })
            .collect();
        json!({ "runs": list })
    }

    fn command_policy(
        &self,
        program: &str,
        arguments: &[String],
    ) -> Result<AgentRun, &'static str> {
        session_command_policy(&self.launch, &self.worktree, program, arguments)
    }

    fn cancel(&self, query: RunCancelRequest) -> Result<Value, String> {
        let (record, cancelled) = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancel(&query.run_id)?;
        Ok(json!({
            "run_id": record.id,
            "cancelled": cancelled,
            "already_finished": !cancelled,
            "phase": record.phase.as_str(),
            "exit_code": record.exit_code,
            "exit_status": record.exit_status,
            "log": record.log,
        }))
    }
}

fn apply_restored_notices(
    results: &mut RunResults,
    restarted: Vec<QueuedRunRecord>,
    lost: Vec<QueuedRunRecord>,
    reasons: &[String],
    pre_shutdown_results: Vec<QueuedRunRecord>,
) {
    if !restarted.is_empty() {
        if !lost.is_empty() {
            let joined = reasons.join("; ");
            results
                .ready
                .push(ContentBlock::from(lost_runs_prompt(&lost, Some(&joined))));
        }
        results.restart_notice = Some(RestartNotice {
            interrupted: restarted,
            pre_shutdown_results,
        });
        results.waiting = true;
    } else if !lost.is_empty() {
        let joined = reasons.join("; ");
        results
            .ready
            .push(ContentBlock::from(lost_runs_prompt_with_results(
                &lost,
                &pre_shutdown_results,
                Some(&joined),
            )));
        results.waiting = true;
    }
}

fn accepted_reply(
    record: &QueuedRunRecord,
    log: PathBuf,
    estimated: QueueEstimate,
) -> QueuedRunAccepted {
    QueuedRunAccepted {
        run_id: record.id.clone(),
        log,
        phase: record.phase.as_str().to_owned(),
        queue_position: estimated.position,
        estimated_wait_seconds: estimated.estimated_wait_seconds,
        estimated_wait: estimated.estimated_wait,
        queue_reason: estimated
            .queue_reason
            .or_else(|| record.queue_reason.clone()),
        change: record.change.clone(),
        head: record.head.clone(),
    }
}

async fn read_excerpt_async(log: Option<&str>) -> Option<String> {
    let path = PathBuf::from(log?);
    tokio::task::spawn_blocking(move || read_log_excerpt(&path))
        .await
        .ok()
        .flatten()
}

fn format_run_status(
    record: &QueuedRunRecord,
    queue_estimate: Option<&QueueEstimate>,
    log_excerpt: Option<String>,
) -> Value {
    let queue_reason = record
        .queue_reason
        .clone()
        .or_else(|| queue_estimate.and_then(|item| item.queue_reason.clone()));
    json!({
        "run_id": record.id,
        "command": record.command_line(),
        "program": record.program,
        "arguments": record.arguments,
        "directory": record.directory,
        "class": record.class.as_str(),
        "phase": record.phase.as_str(),
        "queue_reason": queue_reason,
        "queue_position": queue_estimate.map(|item| item.position),
        "estimated_wait_seconds": queue_estimate.map(|item| item.estimated_wait_seconds),
        "estimated_wait": queue_estimate.map(|item| item.estimated_wait.as_str()),
        "change": record.change,
        "head": record.head,
        "exit_code": record.exit_code,
        "exit_status": record.exit_status,
        "log": record.log,
        "log_excerpt": log_excerpt,
        "result": record.result,
    })
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

fn session_blocks_vcs(launch: &AgentLaunch, worktree: &Path) -> bool {
    launch
        .env_value(BLOCK_VCS_ENV)
        .is_some_and(|value| value == "1")
        || launch.env_value("ROSEN_CHANGE").is_some()
        || launch.env_value("ROSEN_ENDPOINT").is_some()
        || worktree
            .ancestors()
            .any(|path| path.join(".rosen").exists())
}

fn session_command_policy(
    launch: &AgentLaunch,
    worktree: &Path,
    program: &str,
    arguments: &[String],
) -> Result<AgentRun, &'static str> {
    velocity_rosen::agent_run::policy_with_vcs_blocked(
        program,
        arguments,
        session_blocks_vcs(launch, worktree),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#563): queue_run executes inside Velocity's app process, so policy admission must
    // inspect the session's Rosen launch env / worktree to refuse direct `git`/`gh` while
    // admitting ordinary test commands (such as `go -C server test .`).
    #[test]
    fn session_policy_refuses_direct_vcs_in_rosen_and_admits_go_tests() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "velocity-queued-runs-policy-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir)?;

        let mut rosen_launch = AgentLaunch::new(PathBuf::from("agent"));
        rosen_launch.set_env(BLOCK_VCS_ENV, "1");
        let status = vec!["status".to_owned()];
        let go_args = vec![
            "-C".to_owned(),
            "server".to_owned(),
            "test".to_owned(),
            ".".to_owned(),
        ];

        assert!(session_command_policy(&rosen_launch, &dir, "git", &status).is_err());
        assert!(session_command_policy(&rosen_launch, &dir, "gh", &status).is_err());
        assert_eq!(
            session_command_policy(&rosen_launch, &dir, "go", &go_args),
            Ok(AgentRun::Ordinary)
        );

        let plain_launch = AgentLaunch::new(PathBuf::from("agent"));
        assert_eq!(
            session_command_policy(&plain_launch, &dir, "git", &status),
            Ok(AgentRun::Ordinary)
        );

        std::fs::create_dir_all(dir.join(".rosen"))?;
        assert!(session_command_policy(&plain_launch, &dir, "git", &status).is_err());

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
