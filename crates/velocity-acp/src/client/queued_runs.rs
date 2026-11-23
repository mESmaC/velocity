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
    MAX_RUN_HISTORY, QueuedRunClass, QueuedRunPhase, QueuedRunRecord,
    continue_after_restart_prompt, lost_runs_prompt, lost_runs_prompt_with_results,
    restarted_runs_notice,
};
use velocity_rosen::agent_run::{AgentRun, BLOCK_VCS_ENV};
use velocity_rosen::queued_turn::{QUEUE_ENV, QueueEstimate, QueueTicket, estimate};
use velocity_rosen::registered_generator::{
    BASE_CONFIG_TIMEOUT, RegisteredGenerator, is_workspace_root, load_base_commands,
};

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

/// Durable persistence boundary for acknowledged `queue_run` and `run_cancel` transitions (#664).
///
/// Implementations must flush the transition to durable storage (for example `state.json`)
/// before returning `Ok(())`. Returning `Err` rejects the tool request so Velocity never
/// acknowledges a durable `run_id` or cancellation that would be lost on restart.
pub trait QueuedRunPersistence: Send + Sync {
    /// Durably records an accepted queued run before `queue_run` returns its `run_id`.
    fn persist_added(&self, record: &QueuedRunRecord) -> Result<(), String>;

    /// Durably records a cancelled run before `run_cancel` returns `cancelled: true`.
    fn persist_cancelled(&self, record: &QueuedRunRecord) -> Result<(), String>;

    /// Durably records a terminal run record when a concurrent completion wins over `run_cancel`.
    fn persist_finished(&self, record: &QueuedRunRecord) -> Result<(), String>;
}

/// Shared handle to the owning thread's durable queued-run persistence (#664).
#[derive(Clone)]
pub struct QueuedRunStore(Arc<dyn QueuedRunPersistence>);

impl QueuedRunStore {
    pub fn new(backend: Arc<dyn QueuedRunPersistence>) -> Self {
        Self(backend)
    }

    async fn persist_added(&self, record: QueuedRunRecord) -> Result<(), String> {
        let backend = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || backend.persist_added(&record))
            .await
            .map_err(|error| format!("Queued run persistence task failed: {error}"))?
    }

    async fn persist_cancelled(&self, record: QueuedRunRecord) -> Result<(), String> {
        let backend = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || backend.persist_cancelled(&record))
            .await
            .map_err(|error| format!("Queued run persistence task failed: {error}"))?
    }

    async fn persist_finished(&self, record: QueuedRunRecord) -> Result<(), String> {
        let backend = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || backend.persist_finished(&record))
            .await
            .map_err(|error| format!("Queued run persistence task failed: {error}"))?
    }
}

impl std::fmt::Debug for QueuedRunStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("QueuedRunStore(..)")
    }
}

impl PartialEq for QueuedRunStore {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for QueuedRunStore {}

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
        agent_events: &UnboundedSender<AgentEvent>,
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
            persistence: setup.queued_run_store.clone(),
        };
        let mut initial_runs = JoinSet::new();
        let mut results = RunResults {
            outstanding,
            receiver: Some(receiver),
            released: Some(released),
            registry: Some(registry),
            ..RunResults::default()
        };
        host.start_restored(
            restored,
            history,
            &mut initial_runs,
            &mut results,
            agent_events,
        )
        .await;
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
    pub fn registry(&self) -> Option<Arc<Mutex<RunRegistry>>> {
        self.registry.as_ref().map(Arc::clone)
    }

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
            failed.delivered = false;
            self.push_delivered_id(&failed.id);
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
        std::mem::take(&mut self.delivered_ids)
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
    ticket: Option<QueueTicket>,
    cancel: Option<CancellationToken>,
    cancelled_by_user: Arc<AtomicBool>,
}

pub(super) struct RunRegistry {
    runs: Vec<TrackedRun>,
}

impl RunRegistry {
    pub(super) fn from_history(history: &[QueuedRunRecord]) -> Self {
        let runs = history
            .iter()
            .cloned()
            .map(|record| TrackedRun {
                record,
                policy: None,
                ticket: None,
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
            existing.ticket = None;
            existing.cancel = Some(cancel);
            existing.cancelled_by_user = Arc::clone(&cancelled_by_user);
        } else {
            self.runs.push(TrackedRun {
                record,
                policy: Some(policy),
                ticket: None,
                cancel: Some(cancel),
                cancelled_by_user: Arc::clone(&cancelled_by_user),
            });
        }
        cancelled_by_user
    }

    fn set_ticket(&mut self, run_id: &str, ticket: QueueTicket) {
        if let Some(entry) = self.runs.iter_mut().find(|run| run.record.id == run_id)
            && !entry.record.phase.is_terminal()
        {
            entry.ticket = Some(ticket);
        }
    }

    fn clear_ticket(&mut self, run_id: &str) {
        if let Some(entry) = self.runs.iter_mut().find(|run| run.record.id == run_id) {
            entry.ticket = None;
        }
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
            existing.ticket = None;
            existing.cancel = None;
        } else {
            self.runs.push(TrackedRun {
                record,
                policy: None,
                ticket: None,
                cancel: None,
                cancelled_by_user: Arc::new(AtomicBool::new(false)),
            });
        }
        self.trim_history();
    }

    pub(super) fn mark_delivered(&mut self, ids: &[String]) {
        for run in &mut self.runs {
            if ids.iter().any(|id| id == &run.record.id) {
                run.record.delivered = true;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn is_delivered(&self, id: &str) -> bool {
        self.runs
            .iter()
            .find(|run| run.record.id == id)
            .is_some_and(|run| run.record.delivered)
    }

    fn pending_count_before(
        &self,
        run_id: Option<&str>,
        target_policy: AgentRun,
        target_class: QueuedRunClass,
    ) -> usize {
        let mut count = 0;
        for run in &self.runs {
            if run_id.is_some_and(|id| id == run.record.id) {
                break;
            }
            if counts_as_session_pending_ahead(run, target_policy, target_class) {
                count += 1;
            }
        }
        count
    }

    fn snapshot(
        &self,
        run_id: &str,
    ) -> Option<(QueuedRunRecord, Option<AgentRun>, Option<QueueTicket>)> {
        self.runs
            .iter()
            .find(|run| run.record.id == run_id)
            .map(|run| (run.record.clone(), run.policy, run.ticket.clone()))
    }

    fn all_records(&self) -> Vec<QueuedRunRecord> {
        self.runs.iter().map(|run| run.record.clone()).collect()
    }

    fn prepare_cancel(&self, run_id: &str) -> Result<(QueuedRunRecord, bool), String> {
        let Some(entry) = self.runs.iter().find(|run| run.record.id == run_id) else {
            return Err(format!("Unknown run ID: {run_id}"));
        };
        if entry.record.phase.is_terminal() {
            return Ok((entry.record.clone(), false));
        }
        let mut cancelled = entry.record.clone();
        cancelled.phase = QueuedRunPhase::Cancelled;
        cancelled.queue_reason = None;
        cancelled.exit_status = Some("cancelled".into());
        cancelled.delivered = true;
        Ok((cancelled, true))
    }

    fn cancel(&mut self, run_id: &str) -> Result<(QueuedRunRecord, bool), String> {
        let Some(entry) = self.runs.iter_mut().find(|run| run.record.id == run_id) else {
            return Err(format!("Unknown run ID: {run_id}"));
        };
        if entry.record.phase.is_terminal() {
            return Ok((entry.record.clone(), false));
        }
        entry.cancelled_by_user.store(true, Ordering::Release);
        entry.ticket = None;
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

    pub(super) fn set_ticket(&self, ticket: QueueTicket) {
        self.registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_ticket(&self.run_id, ticket);
    }

    pub(super) fn clear_ticket(&self) {
        self.registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear_ticket(&self.run_id);
    }

    pub(super) fn set_running(&self, change: Option<String>, head: Option<String>) {
        let updated = {
            let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
            registry.clear_ticket(&self.run_id);
            registry.update_record(&self.run_id, |record| {
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
            })
        };
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
    persistence: Option<QueuedRunStore>,
}

impl QueueHost {
    async fn start_restored(
        &self,
        restored: Vec<QueuedRunRecord>,
        history: Vec<QueuedRunRecord>,
        runs: &mut JoinSet<RunResult>,
        results: &mut RunResults,
        agent_events: &UnboundedSender<AgentEvent>,
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
            match self.spawn_restored(&record, runs).await {
                Ok(()) => restarted.push(record),
                Err(reason) => {
                    self.record_lost_restored(&record, &reason, agent_events, results);
                    lost.push(record);
                    if !reasons.contains(&reason) {
                        reasons.push(reason);
                    }
                }
            }
        }
        apply_restored_notices(results, restarted, lost, &reasons, pre_shutdown_results);
    }

    fn record_lost_restored(
        &self,
        record: &QueuedRunRecord,
        reason: &str,
        agent_events: &UnboundedSender<AgentEvent>,
        results: &mut RunResults,
    ) {
        let mut failed = record.clone();
        failed.phase = QueuedRunPhase::Failed;
        failed.queue_reason = None;
        failed.exit_status = Some(format!("could not restart: {reason}"));
        failed.delivered = false;
        self.registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .finish(failed.clone());
        results.push_delivered_id(&failed.id);
        let _ = agent_events.send(AgentEvent::QueuedRunFinished(failed));
    }

    async fn spawn_restored(
        &self,
        record: &QueuedRunRecord,
        runs: &mut JoinSet<RunResult>,
    ) -> Result<(), String> {
        let command = RunCommand::from_record(record);
        let (policy, registered) = self.command_policy(&command).await?;
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
            run.finish(
                &rosen, policy, registered, &worktree, &launch, run_stop, sink,
            )
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
                Work::Command(command) => self.enqueue(command, runs).await.map(Accepted::Queued),
                Work::Workflow(workflow) => self.start_workflow(workflow, runs).await,
                Work::Status(query) => self.status(query).await.map(Accepted::Json),
                Work::Cancel(query) => self.cancel(query).await.map(Accepted::Json),
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

    async fn enqueue(
        &self,
        command: RunCommand,
        runs: &mut JoinSet<RunResult>,
    ) -> Result<QueuedRunAccepted, String> {
        let (policy, registered) = self.command_policy(&command).await?;
        let pending_ahead = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending_count_before(None, policy, command.run_class());
        let estimated = self.estimate_queue(policy, command.run_class(), pending_ahead);
        let run = QueuedRun::new(command, &self.worktree, &self.launch)
            .map_err(|error| error.to_string())?;
        let record = run.record();
        let accepted = accepted_reply(&record, run.log.clone(), estimated);
        // Subscribe before acknowledging. A fast turn end must release this command
        // without allowing it to run during the agent's final handoff call.
        let mut turn_ended = self.turn_ended.clone();
        turn_ended.mark_unchanged();
        if self.stop.is_cancelled() {
            let log = run.log.clone();
            drop(run);
            let _ = std::fs::remove_file(&log);
            return Err("The session stopped before the queued run could be saved".into());
        }
        if let Some(store) = &self.persistence
            && let Err(error) = store.persist_added(record.clone()).await
        {
            let log = run.log.clone();
            drop(run);
            let _ = std::fs::remove_file(&log);
            return Err(format!("Could not persist queued run: {error}"));
        }
        let (run_stop, sink) = self.register_run(record.clone(), policy);
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
            run.finish(
                &rosen, policy, registered, &worktree, &launch, run_stop, sink,
            )
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
        let Some((record, policy, ticket)) = self
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
            if let Some(estimate) = ticket.as_ref().and_then(QueueTicket::estimate) {
                Some(estimate)
            } else {
                let resolved_policy = match policy {
                    Some(policy) => Some(policy),
                    None => {
                        let command = RunCommand::from_record(&record);
                        self.command_policy(&command)
                            .await
                            .ok()
                            .map(|(resolved, _)| resolved)
                    }
                };
                resolved_policy.map(|policy| {
                    let pending_ahead = self
                        .registry
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .pending_count_before(Some(&run_id), policy, record.class);
                    self.estimate_queue(policy, record.class, pending_ahead)
                })
            }
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

    async fn command_policy(
        &self,
        command: &RunCommand,
    ) -> Result<(AgentRun, Option<RegisteredGenerator>), String> {
        let directory = resolve_directory(&self.worktree, command.directory.as_deref());
        let at_root = is_workspace_root(&directory, &self.worktree);
        let endpoint = self.launch.env_value("ROSEN_ENDPOINT");
        let generated_commands = if at_root && let Some(endpoint) = endpoint {
            tokio::time::timeout(BASE_CONFIG_TIMEOUT, load_base_commands(endpoint))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default()
        } else {
            Default::default()
        };
        let required_tests = if at_root
            && is_cargo_test_command(command)
            && let Some(endpoint) = endpoint
        {
            tokio::time::timeout(
                BASE_CONFIG_TIMEOUT,
                velocity_rosen::preflight::load_required_tests_for_workspace(
                    endpoint,
                    &self.worktree,
                ),
            )
            .await
            .ok()
            .flatten()
        } else {
            None
        };
        velocity_rosen::agent_run::policy_with_workspace_overrides(
            &command.program,
            &command.arguments,
            session_blocks_vcs(&self.launch, &self.worktree),
            at_root,
            &generated_commands,
            required_tests.as_ref(),
            &|key| command.env.get(key).cloned(),
        )
        .map_err(str::to_owned)
    }

    async fn cancel(&self, query: RunCancelRequest) -> Result<Value, String> {
        let (candidate, needs_cancel) = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .prepare_cancel(&query.run_id)?;
        if !needs_cancel {
            return Ok(cancel_reply(&candidate, false));
        }
        if let Some(store) = &self.persistence {
            store
                .persist_cancelled(candidate)
                .await
                .map_err(|error| format!("Could not persist run cancellation: {error}"))?;
        }
        let (record, cancelled) = self
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancel(&query.run_id)?;
        if !cancelled && let Some(store) = &self.persistence {
            let _ = store.persist_finished(record.clone()).await;
        }
        Ok(cancel_reply(&record, cancelled))
    }
}

fn is_cargo_test_command(command: &RunCommand) -> bool {
    let name = command
        .program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let base = name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".cmd"))
        .or_else(|| name.strip_suffix(".bat"))
        .unwrap_or(&name);
    base == "cargo" && command.arguments.first().map(String::as_str) == Some("test")
}

fn counts_as_session_pending_ahead(
    run: &TrackedRun,
    target_policy: AgentRun,
    target_class: QueuedRunClass,
) -> bool {
    if !matches!(
        run.record.phase,
        QueuedRunPhase::WaitingForTurnEnd | QueuedRunPhase::Queued
    ) {
        return false;
    }
    match target_policy {
        AgentRun::Ordinary => {
            // Ordinary runs that already hold a live Rosen waiting ticket are already counted
            // in `RunQueue::line()`, so counting them again here would double-count them (#665).
            if run.ticket.as_ref().is_some_and(QueueTicket::is_active) {
                return false;
            }
            let same_queue = run.policy.is_none_or(|policy| policy == AgentRun::Ordinary);
            same_queue && class_rank(run.record.class) <= class_rank(target_class)
        }
        AgentRun::RustUtility
        | AgentRun::RustCheck
        | AgentRun::Bindings
        | AgentRun::RegisteredGenerator => {
            run.policy.is_none_or(|policy| policy != AgentRun::Ordinary)
        }
    }
}

fn class_rank(class: QueuedRunClass) -> u8 {
    match class {
        QueuedRunClass::Landing => 0,
        QueuedRunClass::Build => 1,
        QueuedRunClass::Verify => 2,
    }
}

fn cancel_reply(record: &QueuedRunRecord, cancelled: bool) -> Value {
    json!({
        "run_id": record.id,
        "cancelled": cancelled,
        "already_finished": !cancelled,
        "phase": record.phase.as_str(),
        "exit_code": record.exit_code,
        "exit_status": record.exit_status,
        "log": record.log,
    })
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

#[cfg(test)]
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

    // Risk (#663): lost-run notices created before the provider prompt is accepted must stay
    // undelivered so a shutdown or prompt failure before delivery preserves automatic recovery.
    #[test]
    fn lost_runs_stay_undelivered_until_prompt_acceptance() {
        use velocity_core::queued_runs::QueuedRunClass;

        let lost = QueuedRunRecord::new(
            "velocity-run-lost".into(),
            "cargo".into(),
            vec!["check".into(), "-p".into(), "velocity-acp".into()],
            None,
            QueuedRunClass::Build,
        );
        let mut finished = QueuedRunRecord::new(
            "velocity-run-finished".into(),
            "cargo".into(),
            vec!["check".into(), "-p".into(), "velocity-core".into()],
            None,
            QueuedRunClass::Build,
        );
        finished.phase = QueuedRunPhase::Completed;

        let (sender, mut receiver) = unbounded_channel();
        let mut results = RunResults::default();
        results.record_lost_runs(
            &[lost],
            &[finished],
            "the queued run tool is unavailable",
            &sender,
        );

        let event = receiver.try_recv().ok();
        assert!(matches!(
            event,
            Some(AgentEvent::QueuedRunFinished(ref record))
                if record.id == "velocity-run-lost"
                    && record.phase == QueuedRunPhase::Failed
                    && !record.delivered
        ));
        assert!(receiver.try_recv().is_err());
        assert!(results.waiting);
        assert_eq!(results.ready.len(), 1);
        assert_eq!(
            results.take_delivered_ids(),
            vec![
                "velocity-run-lost".to_owned(),
                "velocity-run-finished".to_owned(),
            ]
        );
    }

    // Risk (#663): restored runs that cannot be restarted must persist delivered=false until
    // the provider accepts the lost-run notice prompt, and take_delivered_ids must not mark
    // RunRegistry entries delivered before acceptance.
    #[tokio::test]
    async fn lost_restored_runs_stay_undelivered_until_prompt_acceptance() {
        use velocity_core::queued_runs::QueuedRunClass;

        let lost = QueuedRunRecord::new(
            "velocity-run-lost-restored".into(),
            "cargo".into(),
            vec!["check".into(), "-p".into(), "velocity-acp".into()],
            Some("missing-directory-for-663-test".into()),
            QueuedRunClass::Build,
        );
        let mut pre_shutdown = QueuedRunRecord::new(
            "velocity-run-pre-shutdown".into(),
            "cargo".into(),
            vec!["check".into(), "-p".into(), "velocity-core".into()],
            None,
            QueuedRunClass::Build,
        );
        pre_shutdown.phase = QueuedRunPhase::Completed;

        let worktree = std::env::temp_dir().join(format!(
            "velocity-663-lost-restored-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&worktree).ok();
        let registry = Arc::new(Mutex::new(RunRegistry::from_history(std::slice::from_ref(
            &pre_shutdown,
        ))));
        let (queue_events, receiver) = unbounded_channel();
        let (released, turn_ended) = watch::channel(());
        let outstanding = Arc::new(AtomicUsize::new(0));
        let host = QueueHost {
            token: "test".into(),
            rosen: PathBuf::from("rosen"),
            worktree: worktree.clone(),
            launch: AgentLaunch::new(PathBuf::from("agent")),
            graph_context: None,
            stop: CancellationToken::new(),
            events: queue_events,
            outstanding: Arc::clone(&outstanding),
            turn_ended,
            registry: Arc::clone(&registry),
            persistence: None,
        };
        let mut runs = JoinSet::new();
        let mut results = RunResults {
            outstanding,
            receiver: Some(receiver),
            released: Some(released),
            registry: Some(Arc::clone(&registry)),
            ..RunResults::default()
        };
        let (agent_sender, mut agent_receiver) = unbounded_channel();

        host.start_restored(
            vec![lost],
            vec![pre_shutdown],
            &mut runs,
            &mut results,
            &agent_sender,
        )
        .await;

        let event = agent_receiver.try_recv().ok();
        assert!(matches!(
            event,
            Some(AgentEvent::QueuedRunFinished(ref record))
                if record.id == "velocity-run-lost-restored"
                    && record.phase == QueuedRunPhase::Failed
                    && !record.delivered
        ));
        assert!(agent_receiver.try_recv().is_err());
        assert!(results.waiting);
        assert_eq!(results.ready.len(), 1);
        assert_eq!(
            results.take_delivered_ids(),
            vec![
                "velocity-run-pre-shutdown".to_owned(),
                "velocity-run-lost-restored".to_owned(),
            ]
        );
        let guard = registry.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(!guard.is_delivered("velocity-run-pre-shutdown"));
        assert!(!guard.is_delivered("velocity-run-lost-restored"));
        drop(guard);

        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[derive(Default)]
    struct TestRunPersistence {
        active: Mutex<Vec<QueuedRunRecord>>,
        history: Mutex<Vec<QueuedRunRecord>>,
        fail_add: AtomicBool,
        fail_cancel: AtomicBool,
    }

    impl TestRunPersistence {
        fn snapshot(&self) -> (Vec<QueuedRunRecord>, Vec<QueuedRunRecord>) {
            let active = self
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let history = self
                .history
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            (active, history)
        }
    }

    impl QueuedRunPersistence for TestRunPersistence {
        fn persist_added(&self, record: &QueuedRunRecord) -> Result<(), String> {
            if self.fail_add.load(Ordering::Acquire) {
                return Err("simulated add write failure".into());
            }
            let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
            if !active.iter().any(|existing| existing.id == record.id) {
                active.push(record.clone());
            }
            Ok(())
        }

        fn persist_cancelled(&self, record: &QueuedRunRecord) -> Result<(), String> {
            if self.fail_cancel.load(Ordering::Acquire) {
                return Err("simulated cancel write failure".into());
            }
            self.persist_finished(record)
        }

        fn persist_finished(&self, record: &QueuedRunRecord) -> Result<(), String> {
            self.active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|existing| existing.id != record.id);
            let mut history = self.history.lock().unwrap_or_else(PoisonError::into_inner);
            history.retain(|existing| existing.id != record.id);
            history.push(record.clone());
            Ok(())
        }
    }

    async fn spawn_test_host(
        worktree: &Path,
        restored: Vec<QueuedRunRecord>,
        history: Vec<QueuedRunRecord>,
        persistence: Option<QueuedRunStore>,
    ) -> io::Result<(
        u16,
        CancellationToken,
        JoinHandle<()>,
        RunResults,
        Arc<Mutex<RunRegistry>>,
    )> {
        spawn_test_host_with_launch(
            worktree,
            AgentLaunch::new(PathBuf::from("agent")),
            restored,
            history,
            persistence,
        )
        .await
    }

    async fn spawn_test_host_with_launch(
        worktree: &Path,
        launch: AgentLaunch,
        restored: Vec<QueuedRunRecord>,
        history: Vec<QueuedRunRecord>,
        persistence: Option<QueuedRunStore>,
    ) -> io::Result<(
        u16,
        CancellationToken,
        JoinHandle<()>,
        RunResults,
        Arc<Mutex<RunRegistry>>,
    )> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let (events, receiver) = unbounded_channel();
        let (released, turn_ended) = watch::channel(());
        let stop = CancellationToken::new();
        let outstanding = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(Mutex::new(RunRegistry::from_history(&history)));
        let host = QueueHost {
            token: "test-token".into(),
            rosen: PathBuf::from("rosen"),
            worktree: worktree.to_path_buf(),
            launch,
            graph_context: None,
            stop: stop.clone(),
            events,
            outstanding: Arc::clone(&outstanding),
            turn_ended,
            registry: Arc::clone(&registry),
            persistence,
        };
        let mut initial_runs = JoinSet::new();
        let mut results = RunResults {
            outstanding,
            receiver: Some(receiver),
            released: Some(released),
            registry: Some(Arc::clone(&registry)),
            ..RunResults::default()
        };
        let (agent_sender, _agent_receiver) = unbounded_channel();
        host.start_restored(
            restored,
            history,
            &mut initial_runs,
            &mut results,
            &agent_sender,
        )
        .await;
        let worker = tokio::spawn(host.serve(listener, initial_runs));
        Ok((port, stop, worker, results, registry))
    }

    async fn call_tool(
        port: u16,
        call: fn(u16, &str, Value) -> Result<Value, String>,
        arguments: Value,
    ) -> Result<Value, String> {
        tokio::task::spawn_blocking(move || call(port, "test-token", arguments))
            .await
            .map_err(|error| error.to_string())?
    }

    fn remove_log_files(records: &[QueuedRunRecord]) {
        for record in records {
            if let Some(log) = &record.log {
                let _ = std::fs::remove_file(log);
            }
        }
    }

    // Risk (#664): positive `queue_run` replies must wait for durable persistence so crashing
    // before the ACP/Tauri event pump handles `Added` still recovers the acknowledged `run_id`,
    // while persistence failures reject the tool call without leaking in-memory state.
    #[tokio::test]
    async fn queue_run_persists_before_reply_and_survives_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        let worktree =
            std::env::temp_dir().join(format!("velocity-664-add-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&worktree)?;
        let backend = Arc::new(TestRunPersistence::default());
        let store = QueuedRunStore::new(Arc::clone(&backend) as Arc<dyn QueuedRunPersistence>);
        let args = json!({ "program": "cargo", "arguments": ["check", "-p", "velocity-acp"] });

        let (port, stop, worker, results, registry) =
            spawn_test_host(&worktree, Vec::new(), Vec::new(), Some(store.clone())).await?;
        backend.fail_add.store(true, Ordering::Release);
        assert!(
            call_tool(port, queued_run_tool::request_queued_run, args.clone())
                .await
                .is_err()
        );
        assert!(!results.outstanding());
        assert!(
            registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .all_records()
                .is_empty()
        );

        backend.fail_add.store(false, Ordering::Release);
        let accepted = call_tool(port, queued_run_tool::request_queued_run, args).await?;
        let run_id = accepted["run_id"]
            .as_str()
            .ok_or("missing run_id")?
            .to_owned();
        let (active, history) = backend.snapshot();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, run_id);
        assert!(history.is_empty());

        // Crash before draining QueueEvent::Added; a restarted host must still recover the run.
        stop.cancel();
        let _ = worker.await;
        let (restarted_port, restarted_stop, restarted_worker, restarted_results, _) =
            spawn_test_host(&worktree, active.clone(), history, Some(store)).await?;
        assert!(restarted_results.waiting);
        let status = call_tool(
            restarted_port,
            queued_run_tool::request_run_status,
            json!({ "run_id": run_id }),
        )
        .await?;
        assert_eq!(status["run_id"].as_str(), Some(active[0].id.as_str()));

        restarted_stop.cancel();
        let _ = restarted_worker.await;
        remove_log_files(&active);
        let _ = std::fs::remove_dir_all(&worktree);
        Ok(())
    }

    // Risk (#664): positive `run_cancel` replies must wait for durable persistence so crashing
    // before the finished event reaches the pump never replays the cancelled command on restart,
    // while persistence failures reject `run_cancel` and keep the run active.
    #[tokio::test]
    async fn run_cancel_persists_before_reply_and_prevents_restart_replay()
    -> Result<(), Box<dyn std::error::Error>> {
        let worktree =
            std::env::temp_dir().join(format!("velocity-664-cancel-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&worktree)?;
        let backend = Arc::new(TestRunPersistence::default());
        let store = QueuedRunStore::new(Arc::clone(&backend) as Arc<dyn QueuedRunPersistence>);
        let args = json!({ "program": "cargo", "arguments": ["check", "-p", "velocity-acp"] });

        let (port, stop, worker, _results, _registry) =
            spawn_test_host(&worktree, Vec::new(), Vec::new(), Some(store.clone())).await?;
        let accepted = call_tool(port, queued_run_tool::request_queued_run, args).await?;
        let run_id = accepted["run_id"]
            .as_str()
            .ok_or("missing run_id")?
            .to_owned();

        backend.fail_cancel.store(true, Ordering::Release);
        assert!(
            call_tool(
                port,
                queued_run_tool::request_run_cancel,
                json!({ "run_id": run_id })
            )
            .await
            .is_err()
        );
        assert_eq!(backend.snapshot().0.len(), 1);

        backend.fail_cancel.store(false, Ordering::Release);
        let cancelled = call_tool(
            port,
            queued_run_tool::request_run_cancel,
            json!({ "run_id": run_id }),
        )
        .await?;
        assert_eq!(cancelled["cancelled"].as_bool(), Some(true));
        assert_eq!(cancelled["phase"].as_str(), Some("cancelled"));

        // Crash before draining QueueEvent::Finished; restart must not replay the cancelled run.
        let (active, history) = backend.snapshot();
        assert!(active.is_empty());
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].phase, QueuedRunPhase::Cancelled);
        assert!(history[0].delivered);
        stop.cancel();
        let _ = worker.await;

        let (restarted_port, restarted_stop, restarted_worker, restarted_results, _) =
            spawn_test_host(&worktree, active, history.clone(), Some(store)).await?;
        assert!(!restarted_results.waiting);
        assert!(!restarted_results.outstanding());
        assert!(restarted_results.ready.is_empty());
        let status = call_tool(
            restarted_port,
            queued_run_tool::request_run_status,
            json!({ "run_id": run_id }),
        )
        .await?;
        assert_eq!(status["phase"].as_str(), Some("cancelled"));

        restarted_stop.cancel();
        let _ = restarted_worker.await;
        remove_log_files(&history);
        let _ = std::fs::remove_dir_all(&worktree);
        Ok(())
    }

    async fn wait_for_active_ticket(
        registry: &Arc<Mutex<RunRegistry>>,
        run_id: &str,
    ) -> Result<(), String> {
        for _ in 0..100 {
            let active = registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .snapshot(run_id)
                .and_then(|(_, _, ticket)| ticket)
                .is_some_and(|ticket| ticket.is_active());
            if active {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        Err(format!("timed out waiting for Rosen ticket on {run_id}"))
    }

    async fn queue_position(port: u16, run_id: &str) -> Result<Option<u64>, String> {
        let status = call_tool(
            port,
            queued_run_tool::request_run_status,
            json!({ "run_id": run_id }),
        )
        .await?;
        Ok(status["queue_position"].as_u64())
    }

    fn make_busy_build_queue(root: &Path) -> io::Result<(PathBuf, PathBuf, std::fs::File)> {
        let worktree = root.join("worktree");
        let queue_dir = root.join("queue");
        std::fs::create_dir_all(&worktree)?;
        std::fs::create_dir_all(&queue_dir)?;
        std::fs::write(queue_dir.join("slots"), "build=1\n")?;
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(queue_dir.join("run-build.lock"))?;
        holder.lock()?;
        Ok((worktree, queue_dir, holder))
    }

    async fn enqueue_ordinary(port: u16, label: &str) -> Result<(String, Option<u64>), String> {
        let accepted = call_tool(
            port,
            queued_run_tool::request_queued_run,
            json!({ "program": "echo", "arguments": [label] }),
        )
        .await?;
        let run_id = accepted["run_id"]
            .as_str()
            .ok_or_else(|| format!("missing run_id for {label}"))?
            .to_owned();
        Ok((run_id, accepted["queue_position"].as_u64()))
    }

    // Risk (#665): status for an ordinary run already holding a Rosen waiting ticket must
    // report its ticket's actual place without counting itself or later same-class waiters,
    // and submission estimates must include waiting-for-turn-end runs without double-counting
    // session runs that already hold Rosen tickets.
    #[tokio::test]
    async fn ordinary_run_status_uses_ticket_position_and_avoids_double_counting()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "velocity-665-queue-position-{}",
            uuid::Uuid::new_v4()
        ));
        let (worktree, queue_dir, holder) = make_busy_build_queue(&root)?;
        let mut launch = AgentLaunch::new(PathBuf::from("agent"));
        launch.set_env(QUEUE_ENV, queue_dir.to_string_lossy().as_ref());
        let (port, stop, worker, results, registry) =
            spawn_test_host_with_launch(&worktree, launch, Vec::new(), Vec::new(), None).await?;

        let (first_id, first_pos) = enqueue_ordinary(port, "first").await?;
        assert_eq!(first_pos, Some(1));

        results.release();
        wait_for_active_ticket(&registry, &first_id).await?;
        assert_eq!(queue_position(port, &first_id).await?, Some(1));

        let (second_id, second_pos) = enqueue_ordinary(port, "second").await?;
        assert_eq!(second_pos, Some(2));
        let (third_id, third_pos) = enqueue_ordinary(port, "third").await?;
        assert_eq!(third_pos, Some(3));

        results.release();
        wait_for_active_ticket(&registry, &second_id).await?;
        wait_for_active_ticket(&registry, &third_id).await?;
        assert_eq!(queue_position(port, &first_id).await?, Some(1));
        let mut later_positions = vec![
            queue_position(port, &second_id).await?,
            queue_position(port, &third_id).await?,
        ];
        later_positions.sort();
        assert_eq!(later_positions, vec![Some(2), Some(3)]);

        for run_id in [&first_id, &second_id, &third_id] {
            let _ = call_tool(
                port,
                queued_run_tool::request_run_cancel,
                json!({ "run_id": run_id }),
            )
            .await;
        }
        let records = registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .all_records();
        stop.cancel();
        let _ = worker.await;
        drop(holder);
        remove_log_files(&records);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
