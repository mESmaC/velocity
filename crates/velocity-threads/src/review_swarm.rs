//! Post-land reviews beside build threads (#382). This runner is independent of the
//! checker, submits receipts, or owns landing locks. Its only limit is its saved roster.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use velocity_acp::client::{AgentInput, AircLaunch};
use velocity_core::duty::{
    DutyEventKind, DutyFeedEvent, ReviewSwarmCoverage, review_window_seconds,
};
use velocity_core::scout_sweep::nothing_changed_since;
use velocity_rosen::{ProjectServer, review_swarm};
use velocity_store::duty::ScoutRoster;

use crate::review_swarm_server::SwarmLaunch;
use crate::sweep_runner::SweepHost;

pub(crate) type WorkerKey = (u32, String);

struct Running {
    input: UnboundedSender<AgentInput>,
    pending: usize,
}

#[derive(Default)]
struct State {
    running: BTreeMap<WorkerKey, Running>,
    planning: BTreeSet<u32>,
    retry: BTreeMap<WorkerKey, Instant>,
    instructions: BTreeMap<u32, String>,
    no_change_review: BTreeSet<u32>,
    transcripts: BTreeMap<WorkerKey, crate::transcript_view::LiveTranscript>,
}

#[derive(Clone)]
pub(crate) struct ReviewSwarms {
    state: Arc<Mutex<State>>,
    pub executable: PathBuf,
    pub app_data: PathBuf,
    pub reports: PathBuf,
    pub airc: Option<AircLaunch>,
}

impl ReviewSwarms {
    pub fn new(executable: PathBuf, reports: &Path) -> Self {
        Self {
            state: Arc::default(),
            executable,
            app_data: reports.parent().unwrap_or(reports).into(),
            reports: reports.join("review-swarm"),
            airc: None,
        }
    }

    pub fn transcript(
        &self,
        project: u32,
        nickname: &str,
    ) -> Option<Vec<velocity_core::thread_view_model::TranscriptBlock>> {
        self.lock()
            .transcripts
            .get(&(project, nickname.into()))
            .map(crate::transcript_view::LiveTranscript::blocks)
    }

    pub fn start_transcript(&self, key: &WorkerKey, workspace: &Path) {
        let mut state = self.lock();
        let transcript = crate::transcript_view::LiveTranscript::next_work_session(
            workspace,
            state.transcripts.get(key),
        );
        state.transcripts.insert(key.clone(), transcript);
    }

    pub fn record_transcript(&self, key: &WorkerKey, event: &velocity_acp::client::AgentEvent) {
        if let velocity_acp::client::AgentEvent::Transcript { placement, update } = event
            && let Some(transcript) = self.lock().transcripts.get_mut(key)
        {
            transcript.record(*placement, update);
        }
    }

    pub fn instruct(&self, project: u32, instruction: &str) {
        let mut state = self.lock();
        for ((owner, _), running) in &mut state.running {
            if *owner == project && running.input.send(message(instruction)).is_ok() {
                running.pending += 1;
            }
        }
        state.instructions.insert(project, instruction.into());
    }

    pub fn request_no_change_review(&self, project: u32) {
        self.lock().no_change_review.insert(project);
    }

    pub fn dismiss(&self, project: u32, nickname: &str) {
        if let Some(running) = self.lock().running.get(&(project, nickname.into())) {
            let _ = running.input.send(AgentInput::Stop);
        }
    }

    pub fn running(&self) -> BTreeSet<WorkerKey> {
        self.lock().running.keys().cloned().collect()
    }

    /// Killing sessions for removed rosters also handles a deleted project and a stopped
    /// role. Pausing deliberately lets existing reviews finish.
    pub fn retain(&self, rosters: &[ScoutRoster]) {
        for ((project, nickname), running) in &self.lock().running {
            if !rosters.iter().any(|roster| {
                roster.project.id == *project
                    && roster
                        .scouts
                        .iter()
                        .any(|agent| agent.nickname == *nickname)
            }) {
                let _ = running.input.send(AgentInput::Stop);
            }
        }
    }

    /// Planning and network reads happen on a separate task per project. A slow or
    /// unreachable server cannot stall sweeps on other projects, much less a land.
    pub fn schedule<Host: SweepHost>(&self, host: &Host, roster: &ScoutRoster) {
        if !self.lock().planning.insert(roster.project.id) {
            return;
        }
        let (runner, host, roster) = (self.clone(), host.clone(), roster.clone());
        tokio::spawn(async move {
            if let Err(error) = runner.plan(&host, &roster).await {
                coverage_error(&host, roster.project.id, error);
            }
            runner.lock().planning.remove(&roster.project.id);
        });
    }

    async fn plan<Host: SweepHost>(&self, host: &Host, roster: &ScoutRoster) -> Result<(), String> {
        let server = ProjectServer::new(&self.app_data, roster.project.id);
        if !velocity_rosen::post_land::after_land(&server)
            .await
            .map_err(|error| error.to_string())?
        {
            return Ok(());
        }
        let pending = review_swarm::unreviewed(&server)
            .await
            .map_err(|error| error.to_string())?;
        let oldest = pending
            .iter()
            .map(|change| now().saturating_sub(change.landed_at))
            .max()
            .unwrap_or(0);
        coverage(host, roster.project.id, pending.len(), oldest);
        if roster.paused {
            return Ok(());
        }
        std::fs::create_dir_all(&self.reports).map_err(|error| error.to_string())?;
        let window = review_window_seconds(roster.rules.cadence);
        // Initially allow two minutes per review; an ageing backlog uses every slot.
        let remaining = window.saturating_sub(oldest).max(1);
        let desired = (pending.len() as u64)
            .saturating_mul(120)
            .div_ceil(remaining) as usize;
        let cap = desired.min(roster.scouts.len());
        self.start_available(host, roster, &server, &pending, cap)
            .await
    }

    async fn start_available<Host: SweepHost>(
        &self,
        host: &Host,
        roster: &ScoutRoster,
        server: &ProjectServer,
        pending: &[review_swarm::LandedChange],
        cap: usize,
    ) -> Result<(), String> {
        for (index, agent) in roster.scouts.iter().enumerate() {
            let key = (roster.project.id, agent.nickname.clone());
            if !self.available(&key) {
                continue;
            }
            if !worker_present(host, &key) {
                continue;
            }
            let path = self.job_path(&key);
            let recovered = if path.is_file() {
                Some(SwarmLaunch::read(&path)?)
            } else {
                None
            };
            let target = match &recovered {
                // Older scout jobs stay parked without landed work. A no-change
                // review has an issue to verify even without a land of its own.
                Some(job) if job.target.is_some() || job.no_change_only || !pending.is_empty() => {
                    job.target.clone()
                }
                None if self.active_count(key.0) < cap => {
                    let Some(target) = self.claim_next(server, &key.1, pending).await? else {
                        continue;
                    };
                    Some(target)
                }
                None if pending.is_empty()
                    && index == 0
                    && self.lock().no_change_review.contains(&key.0) =>
                {
                    None
                }
                Some(_) => continue,
                None => continue,
            };
            self.start(host, roster, index, target, recovered);
        }
        Ok(())
    }

    async fn claim_next(
        &self,
        server: &ProjectServer,
        nickname: &str,
        pending: &[review_swarm::LandedChange],
    ) -> Result<Option<review_swarm::LandedChange>, String> {
        let member = review_swarm::membership(server, nickname)
            .await
            .map_err(|error| error.to_string())?;
        for target in pending {
            let piece = review_swarm::review_piece(&target.change, &target.commit);
            if review_swarm::claim(&member, &piece, &target.summary)
                .await
                .map_err(|error| error.to_string())?
            {
                return Ok(Some(target.clone()));
            }
        }
        Ok(None)
    }

    fn start<Host: SweepHost>(
        &self,
        host: &Host,
        roster: &ScoutRoster,
        index: usize,
        target: Option<review_swarm::LandedChange>,
        recovered: Option<SwarmLaunch>,
    ) {
        let key = (roster.project.id, roster.scouts[index].nickname.clone());
        let (input, inputs) = unbounded_channel();
        let mut state = self.lock();
        let instruction = state.instructions.get(&key.0).cloned();
        state.running.insert(
            key.clone(),
            Running {
                input: input.clone(),
                pending: 0,
            },
        );
        let no_change_only =
            target.is_none() && recovered.is_none() && state.no_change_review.remove(&key.0);
        drop(state);
        let job = crate::review_swarm_session::Job::new(
            self.clone(),
            roster.clone(),
            index,
            target,
            recovered,
            no_change_only,
        );
        let (runner, host) = (self.clone(), host.clone());
        tokio::spawn(async move {
            let result = job.run(&host, input, inputs, instruction).await;
            let mut state = runner.lock();
            state.running.remove(&key);
            if result.is_err() {
                state.retry.insert(key.clone(), Instant::now());
            }
            drop(state);
            if let Err(error) = result {
                worker_event(
                    &host,
                    &key,
                    false,
                    format!("review waits · {error}"),
                    DutyEventKind::Wait,
                );
            }
        });
    }

    pub fn end_turn(&self, key: &WorkerKey) {
        if let Some(running) = self.lock().running.get_mut(key) {
            if running.pending > 0 {
                running.pending -= 1;
            } else {
                let _ = running.input.send(AgentInput::Stop);
            }
        }
    }

    pub fn job_path(&self, key: &WorkerKey) -> PathBuf {
        self.reports.join(format!("{}-{}.json", key.0, key.1))
    }

    fn available(&self, key: &WorkerKey) -> bool {
        let state = self.lock();
        !state.running.contains_key(key)
            && !state
                .retry
                .get(key)
                .is_some_and(|last| last.elapsed() < Duration::from_secs(60))
    }

    fn active_count(&self, project: u32) -> usize {
        self.lock()
            .running
            .keys()
            .filter(|(owner, _)| *owner == project)
            .count()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn worker_present<Host: SweepHost>(host: &Host, key: &WorkerKey) -> bool {
    host.rosters().iter().any(|current| {
        current.project.id == key.0
            && !current.paused
            && current
                .scouts
                .iter()
                .any(|current| current.nickname == key.1)
    })
}

fn coverage<Host: SweepHost>(host: &Host, project: u32, count: usize, oldest: u64) {
    host.update(
        project,
        Box::new(move |entry, _| {
            entry.review_swarm = Some(ReviewSwarmCoverage::new(count, oldest));
            if count == 0 && !entry.paused {
                for agent in entry.agents.iter_mut().filter(|agent| !agent.busy) {
                    agent.doing = agent.sweep.as_ref().map_or_else(
                        || "waiting for a land".into(),
                        |last| nothing_changed_since(last.started_at),
                    );
                }
            }
            Vec::new()
        }),
    );
}

fn coverage_error<Host: SweepHost>(host: &Host, project: u32, error: String) {
    host.update(
        project,
        Box::new(move |entry, timestamp| {
            let message = format!("Review coverage unavailable · {error}");
            if entry
                .feed
                .last()
                .is_some_and(|event| event.message == message)
            {
                return Vec::new();
            }
            vec![DutyFeedEvent::new(
                timestamp,
                "velocity",
                DutyEventKind::Wait,
                message,
            )]
        }),
    );
}

pub(crate) fn worker_event<Host: SweepHost>(
    host: &Host,
    key: &WorkerKey,
    busy: bool,
    doing: String,
    kind: DutyEventKind,
) -> bool {
    let nickname = key.1.clone();
    host.update(
        key.0,
        Box::new(move |entry, timestamp| {
            let Some(card) = entry
                .agents
                .iter_mut()
                .find(|card| card.nickname == nickname)
            else {
                return Vec::new();
            };
            card.busy = busy;
            card.doing = doing.clone();
            vec![DutyFeedEvent::new(timestamp, &nickname, kind, doing)]
        }),
    )
}

pub(crate) fn message(text: &str) -> AgentInput {
    AgentInput::Send(vec![agent_client_protocol::v1::schema::ContentBlock::from(
        text.to_owned(),
    )])
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
