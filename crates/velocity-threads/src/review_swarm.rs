//! Post-land reviews beside build threads (#382). This runner never enters PR duty's
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
use velocity_core::scout_sweep::{SweepSchedule, sweep_due};
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
    instructed_idle: BTreeSet<u32>,
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

    pub fn instruct(&self, project: u32, instruction: &str) {
        let mut state = self.lock();
        let mut delivered = false;
        for ((owner, _), running) in &mut state.running {
            if *owner == project && running.input.send(message(instruction)).is_ok() {
                running.pending += 1;
                delivered = true;
            }
        }
        state.instructions.insert(project, instruction.into());
        if !delivered {
            state.instructed_idle.insert(project);
        }
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
        let base = velocity_rosen::policy::policy_at(&server, None)
            .await
            .map_err(|error| error.to_string())?
            .base;
        let window = review_window_seconds(roster.rules.cadence);
        // Initially allow two minutes per review; an ageing backlog uses every slot.
        let remaining = window.saturating_sub(oldest).max(1);
        let desired = (pending.len() as u64)
            .saturating_mul(120)
            .div_ceil(remaining)
            .max(1) as usize;
        let cap = desired.min(roster.scouts.len());
        self.start_available(host, roster, &server, &pending, &base, cap)
            .await
    }

    async fn start_available<Host: SweepHost>(
        &self,
        host: &Host,
        roster: &ScoutRoster,
        server: &ProjectServer,
        pending: &[review_swarm::LandedChange],
        base: &str,
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
                Some(job) => job.target.clone(),
                None if self.active_count(key.0) < cap => {
                    self.claim_next(server, &key.1, pending).await?
                }
                None => continue,
            };
            if target.is_none()
                && recovered.is_none()
                && (!pending.is_empty()
                    || index != 0
                    || (!scouting_due(roster, index, base)
                        && !self.lock().instructed_idle.contains(&key.0)))
            {
                continue;
            }
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
        let instruction = self.lock().instructions.get(&key.0).cloned();
        self.lock().running.insert(
            key.clone(),
            Running {
                input: input.clone(),
                pending: 0,
            },
        );
        self.lock().instructed_idle.remove(&key.0);
        let job = crate::review_swarm_session::Job::new(
            self.clone(),
            roster.clone(),
            index,
            target,
            recovered,
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

fn scouting_due(roster: &ScoutRoster, index: usize, base: &str) -> bool {
    let agent = &roster.scouts[index];
    sweep_due(&SweepSchedule {
        cadence: roster.rules.cadence,
        paused: roster.paused,
        running: false,
        last: agent.sweep.as_ref(),
        now: now(),
        base_commit: Some(base),
        interrupted: agent.busy,
    })
    .is_some()
}

fn coverage<Host: SweepHost>(host: &Host, project: u32, count: usize, oldest: u64) {
    host.update(
        project,
        Box::new(move |entry, _| {
            entry.review_swarm = Some(ReviewSwarmCoverage::new(count, oldest));
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
