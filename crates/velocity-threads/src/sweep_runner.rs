//! Running scout sweeps (SPEC.md F7 Scouts): every scout sweeps once when it joins, then on
//! its cadence (once, every 30 m, after every merge) until its role is paused. A sweep runs
//! one agent turn in the scout's `<scout>-sweep` worktree, its card follows the agent's
//! steps, and what it files through the scout MCP server lands in the activity feed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent_client_protocol::v1::schema::ContentBlock;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use velocity_acp::client::{AgentEvent, AgentInput, AircLaunch};
use velocity_core::duty::{
    DutyAgentCard, DutyEventKind, DutyFeedEvent, DutyRoleEntry, ScoutCadence,
};
use velocity_core::scout_sweep::{
    SweepBrief, SweepReason, SweepRecord, SweepSchedule, step_doing, sweep_due,
    sweep_finished_message, sweep_idle_doing, sweep_prompt, sweep_started_message,
};
use velocity_github::{ScoutReport, read_scout_reports};
use velocity_store::duty::ScoutRoster;

use crate::RunningSession;
use crate::sweep::{SweepPlan, plan_sweep, sweep_doing, sweep_session};

/// How often the runner looks for due sweeps when nothing wakes it sooner.
const TICK: Duration = Duration::from_secs(20);
/// How often "after every merge" asks the remote for the base branch's tip.
const BASE_POLL: Duration = Duration::from_secs(60);
/// A card that says this has a sweep that failed; it keeps saying so until the next sweep.
const FAILED: &str = "sweep failed";

/// A change to a project's Scouts: given the feed's timestamp, it returns the events to post.
pub type ScoutChange = Box<dyn FnOnce(&mut DutyRoleEntry, &str) -> Vec<DutyFeedEvent> + Send>;

/// Where the runner saves and shows what sweeps do: the store, the window and its events.
pub trait SweepHost: Clone + Send + Sync + 'static {
    /// Every project's Scouts role.
    fn rosters(&self) -> Vec<ScoutRoster>;
    /// Applies `change` to the project's Scouts, appends and publishes the events it returns,
    /// saves, and refreshes the window. `false` when the project or the role is gone.
    fn update(&self, project: u32, change: ScoutChange) -> bool;
}

type ScoutKey = (u32, String);

/// Your instruction for a project's scouts, and the scouts that already have it.
struct Instruction {
    text: String,
    delivered: BTreeSet<String>,
}

#[derive(Default)]
struct RunnerState {
    /// Each running sweep's input, and how many of your instructions it still has to answer.
    running: BTreeMap<ScoutKey, (UnboundedSender<AgentInput>, usize)>,
    instructions: BTreeMap<u32, Instruction>,
    /// The base branch's tip per project, and when it was read.
    bases: BTreeMap<u32, (Instant, Option<String>)>,
    /// Counts dismissals, so a tick that read its rosters before one starts no sweep for the
    /// dismissed scout.
    dismissals: u64,
}

/// The sweep runner. Cheap to clone; every clone drives the same sweeps.
#[derive(Clone)]
pub struct ScoutSweeps {
    state: Arc<Mutex<RunnerState>>,
    wake: Arc<Notify>,
    /// The executable that serves the scout MCP server (Velocity itself).
    scout_server: PathBuf,
    /// The folder each sweep's report file goes in.
    reports: PathBuf,
    /// The project's AIRC channel, passed to every sweep's agent (F14).
    airc: Option<AircLaunch>,
}

impl ScoutSweeps {
    pub fn new(scout_server: PathBuf, reports: PathBuf) -> Self {
        Self {
            state: Arc::default(),
            wake: Arc::new(Notify::new()),
            scout_server,
            reports,
            airc: None,
        }
    }

    /// Gives every sweep's agent the project's AIRC channel under its scout nick (F14).
    pub fn with_airc(mut self, airc: AircLaunch) -> Self {
        self.airc = Some(airc);
        self
    }

    /// Looks for due sweeps now, e.g. after scouts were started or resumed.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Delivers your instruction from the Duty view's composer (F7: it instructs every agent
    /// in the role). A running sweep gets it as its next message; idle scouts sweep with it.
    pub fn instruct(&self, project: u32, instruction: &str) {
        let mut state = self.lock();
        let mut delivered = BTreeSet::new();
        for ((owner, nickname), (input, pending)) in state.running.iter_mut() {
            if *owner == project && input.send(text(instruction)).is_ok() {
                *pending += 1;
                delivered.insert(nickname.clone());
            }
        }
        let instruction = Instruction {
            text: instruction.to_owned(),
            delivered,
        };
        state.instructions.insert(project, instruction);
        drop(state);
        self.wake();
    }

    /// Dismisses `scout` (F7): stops its running sweep's session, which kills the agent's
    /// process tree. Call it after the scout left the saved roster.
    pub fn dismiss(&self, project: u32, scout: &str) {
        let mut state = self.lock();
        state.dismissals = state.dismissals.wrapping_add(1);
        if let Some((input, _)) = state.running.get(&(project, scout.to_owned())) {
            let _ = input.send(AgentInput::Stop);
        }
        drop(state);
        self.wake();
    }

    /// Scouts whose sweeps are running.
    pub fn running(&self) -> BTreeSet<ScoutKey> {
        self.lock().running.keys().cloned().collect()
    }

    /// Runs until the app exits.
    pub async fn run<Host: SweepHost>(self, host: Host) {
        loop {
            self.tick(&host).await;
            // Woken early or timed out: either way, look again.
            let _ = tokio::time::timeout(TICK, self.wake.notified()).await;
        }
    }

    /// Starts every due sweep once, and settles idle cards.
    pub async fn tick<Host: SweepHost>(&self, host: &Host) {
        let dismissals = self.lock().dismissals;
        for roster in host.rosters() {
            let base = self.base_commit(&roster).await;
            if self.lock().dismissals != dismissals {
                // A scout left while the tip was read: the dismissal's wake reads afresh.
                return;
            }
            let instruction = self.lock().instructions.remove(&roster.project.id);
            for (index, scout) in roster.scouts.iter().enumerate() {
                let key = (roster.project.id, scout.nickname.clone());
                let running = self.lock().running.contains_key(&key);
                let schedule = SweepSchedule {
                    cadence: roster.rules.cadence,
                    paused: roster.paused,
                    running,
                    last: scout.sweep.as_ref(),
                    now: unix_now(),
                    base_commit: base.as_deref(),
                };
                let instructed = instruction
                    .as_ref()
                    .filter(|wanted| !wanted.delivered.contains(&scout.nickname))
                    .filter(|_| !roster.paused && !running)
                    .map(|wanted| wanted.text.clone());
                let reason = match &instructed {
                    Some(_) => Some(SweepReason::Instruction),
                    None => sweep_due(&schedule),
                };
                match reason {
                    Some(reason) => {
                        let brief = SweepStart {
                            index,
                            reason,
                            instruction: instructed,
                            base_hint: base.clone(),
                        };
                        self.start(host, &roster, brief);
                    }
                    None if !running => settle_card(host, &roster, scout),
                    None => {}
                }
            }
        }
    }

    /// The remote base branch's tip, read at most once a minute, and only for "after every
    /// merge", the one cadence that needs it before a sweep starts.
    async fn base_commit(&self, roster: &ScoutRoster) -> Option<String> {
        if roster.rules.cadence != ScoutCadence::AfterEveryMerge || roster.paused {
            return None;
        }
        let project = roster.project.id;
        if let Some((read, commit)) = self.lock().bases.get(&project)
            && read.elapsed() < BASE_POLL
        {
            return commit.clone();
        }
        let repository = PathBuf::from(&roster.project.repository_path);
        let branch = crate::launch::base_branch(&roster.project);
        let commit = tokio::task::spawn_blocking(move || {
            velocity_git::sweep_base(&repository, &branch).map(|(_, commit)| commit)
        })
        .await
        .ok()
        .flatten();
        self.lock()
            .bases
            .insert(project, (Instant::now(), commit.clone()));
        commit
    }

    fn start<Host: SweepHost>(&self, host: &Host, roster: &ScoutRoster, start: SweepStart) {
        let Some(scout) = roster.scouts.get(start.index) else {
            return;
        };
        let key = (roster.project.id, scout.nickname.clone());
        let (input, inputs) = unbounded_channel();
        self.lock().running.insert(key.clone(), (input.clone(), 0));
        let sweep = Sweep {
            runner: self.clone(),
            key,
            roster: roster.clone(),
            start,
            input,
        };
        let host = host.clone();
        tokio::spawn(async move {
            let key = sweep.key.clone();
            let runner = sweep.runner.clone();
            sweep.run(&host, inputs).await;
            runner.lock().running.remove(&key);
            runner.wake();
        });
    }

    fn lock(&self) -> MutexGuard<'_, RunnerState> {
        // The state holds plain maps; a panic elsewhere leaves them usable.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Why and how one scout's sweep starts.
struct SweepStart {
    index: usize,
    reason: SweepReason,
    instruction: Option<String>,
    /// The base tip the runner last read, recorded if the sweep fails before reading its own.
    base_hint: Option<String>,
}

/// One sweep, from its worktree to its last report line.
struct Sweep {
    runner: ScoutSweeps,
    key: ScoutKey,
    roster: ScoutRoster,
    start: SweepStart,
    input: UnboundedSender<AgentInput>,
}

impl Sweep {
    async fn run<Host: SweepHost>(self, host: &Host, inputs: UnboundedReceiver<AgentInput>) {
        set_card(host, &self.key, true, "preparing the sweep worktree".into());
        let (plan, worktree, base_commit) = match self.prepare().await {
            Ok(prepared) => prepared,
            Err(error) => return self.finish(host, false, Some(&error), (0, 0)),
        };
        let report = self
            .runner
            .reports
            .join(format!("{}-{}.jsonl", self.key.0, self.key.1));
        // A report left by an earlier run of this sweep would be read as new filings.
        let _ = std::fs::remove_file(&report);
        let record = SweepRecord::new(unix_now(), base_commit, worktree.display().to_string());
        let message = sweep_started_message(&self.start.reason, &self.roster.rules);
        let nickname = self.key.1.clone();
        let started = host.update(
            self.key.0,
            Box::new(move |entry, timestamp| {
                if let Some(card) = card_mut(entry, &nickname) {
                    card.sweep = Some(record);
                    card.busy = true;
                    "starting the agent".clone_into(&mut card.doing);
                }
                vec![DutyFeedEvent::new(
                    timestamp,
                    &nickname,
                    DutyEventKind::Sweep,
                    message,
                )]
            }),
        );
        if !started {
            return;
        }
        let mut setup = sweep_session(&plan, worktree, &self.runner.scout_server, &report);
        setup.airc = self
            .runner
            .airc
            .as_ref()
            .map(|airc| airc.duty(plan.project, &plan.nickname));
        let _ = self.input.send(text(&self.prompt(&plan)));
        let session = crate::spawn_session(plan.agent.clone(), setup, inputs);
        let (counts, failure) = self.follow(host, session, &report).await;
        let _ = std::fs::remove_file(&report);
        self.finish(host, true, failure.as_deref(), counts);
    }

    /// Makes or resets the sweep worktree at the base branch's tip (blocking git work).
    async fn prepare(&self) -> Result<(SweepPlan, PathBuf, Option<String>), String> {
        let roster = self.roster.clone();
        let index = self.start.index;
        tokio::task::spawn_blocking(move || {
            let scout = roster.scouts.get(index).ok_or("The scout left")?;
            let repository = PathBuf::from(&roster.project.repository_path);
            let branch = crate::launch::base_branch(&roster.project);
            let (base_ref, commit) = velocity_git::sweep_base(&repository, &branch)
                .ok_or_else(|| format!("Could not find the base branch `{branch}`"))?;
            let plan = plan_sweep(&roster, scout, &base_ref).map_err(|error| error.to_string())?;
            let worktree = velocity_git::prepare_sweep_worktree(&plan.worktree)
                .map_err(|error| error.to_string())?;
            Ok((plan, worktree, Some(commit)))
        })
        .await
        .map_err(|error| error.to_string())?
    }

    fn prompt(&self, plan: &SweepPlan) -> String {
        sweep_prompt(&SweepBrief {
            nickname: &self.key.1,
            scout_index: self.start.index,
            scout_count: self.roster.scouts.len(),
            rules: &self.roster.rules,
            reason: &self.start.reason,
            base_branch: &plan.base_branch,
            instruction: self.start.instruction.as_deref(),
            stress_diff_input_count: self.roster.settings.proofs.stress_diff_input_count,
        })
    }

    /// Follows the session to the end of its last turn: the card shows each step, and each
    /// report line becomes a feed event. Returns (filed, skipped) and why it stopped early.
    async fn follow<Host: SweepHost>(
        &self,
        host: &Host,
        mut session: RunningSession,
        report: &Path,
    ) -> ((usize, usize), Option<String>) {
        let mut offset = 0;
        let mut counts = (0, 0);
        let mut doing = String::new();
        while let Some(event) = session.events.recv().await {
            let mut batch = vec![event];
            while let Ok(next) = session.events.try_recv() {
                batch.push(next);
            }
            if let Some(step) = batch.iter().rev().find_map(sweep_doing)
                && step != doing
            {
                set_card(host, &self.key, true, step.clone());
                doing = step;
            }
            offset = self.read_reports(host, report, offset, &mut counts);
            if batch
                .iter()
                .any(|event| matches!(event, AgentEvent::TurnEnded(_)))
            {
                self.end_turn();
            }
        }
        self.read_reports(host, report, offset, &mut counts);
        let failure = match session.outcome.await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some("its session ended unexpectedly".into()),
        };
        (counts, failure)
    }

    /// A turn ended: the sweep is over unless you sent an instruction it has yet to answer.
    fn end_turn(&self) {
        let mut state = self.runner.lock();
        let Some((input, pending)) = state.running.get_mut(&self.key) else {
            return;
        };
        if *pending > 0 {
            *pending -= 1;
        } else {
            let _ = input.send(AgentInput::Stop);
        }
    }

    fn read_reports<Host: SweepHost>(
        &self,
        host: &Host,
        report: &Path,
        offset: u64,
        counts: &mut (usize, usize),
    ) -> u64 {
        let Ok((reports, next)) = read_scout_reports(report, offset) else {
            return offset;
        };
        if reports.is_empty() {
            return next;
        }
        for report in &reports {
            match report {
                ScoutReport::Filed { .. } => counts.0 += 1,
                ScoutReport::Duplicate { .. } => counts.1 += 1,
            }
        }
        let nickname = self.key.1.clone();
        host.update(
            self.key.0,
            Box::new(move |entry, timestamp| {
                reports
                    .into_iter()
                    .map(|report| report_event(entry, timestamp, &nickname, report))
                    .collect()
            }),
        );
        next
    }

    /// The card goes idle and says when the scout sweeps next, or why this sweep failed.
    fn finish<Host: SweepHost>(
        &self,
        host: &Host,
        started: bool,
        failure: Option<&str>,
        counts: (usize, usize),
    ) {
        let nickname = self.key.1.clone();
        let message = sweep_finished_message(counts.0, counts.1, failure);
        let failed = failure.map(|failure| step_doing(&format!("{FAILED} · {failure}")));
        let cadence = self.roster.rules.cadence;
        let base_hint = self.start.base_hint.clone();
        host.update(
            self.key.0,
            Box::new(move |entry, timestamp| {
                let paused = entry.paused;
                let Some(card) = card_mut(entry, &nickname) else {
                    return Vec::new();
                };
                if !started {
                    // The attempt counts as the sweep, so a failing setup is retried on the
                    // cadence, not on every tick.
                    let worktree = card.sweep.take().map(|last| last.worktree);
                    card.sweep = Some(SweepRecord::new(
                        unix_now(),
                        base_hint,
                        worktree.unwrap_or_default(),
                    ));
                }
                card.busy = false;
                card.doing = match (failed, &card.sweep) {
                    (Some(failed), _) => failed,
                    (None, Some(last)) => sweep_idle_doing(cadence, paused, last),
                    (None, None) => "idle".into(),
                };
                vec![DutyFeedEvent::new(
                    timestamp,
                    &nickname,
                    DutyEventKind::Sweep,
                    message,
                )]
            }),
        );
    }
}

/// An idle scout's card: not busy, and saying when it sweeps next. Also clears a card a
/// closed app left busy mid-sweep.
fn settle_card<Host: SweepHost>(
    host: &Host,
    roster: &ScoutRoster,
    scout: &velocity_store::duty::DutyAgentItem,
) {
    let wanted = match &scout.sweep {
        Some(last) => sweep_idle_doing(roster.rules.cadence, roster.paused, last),
        None if roster.paused => "paused".into(),
        None => return,
    };
    let failed = scout.doing.starts_with(FAILED) && !roster.paused;
    if !scout.busy && (scout.doing == wanted || failed) {
        return;
    }
    set_card(
        host,
        &(roster.project.id, scout.nickname.clone()),
        false,
        wanted,
    );
}

fn set_card<Host: SweepHost>(host: &Host, key: &ScoutKey, busy: bool, doing: String) {
    let nickname = key.1.clone();
    host.update(
        key.0,
        Box::new(move |entry, _| {
            if let Some(card) = card_mut(entry, &nickname) {
                card.busy = busy;
                card.doing = doing;
            }
            Vec::new()
        }),
    );
}

fn card_mut<'entry>(
    entry: &'entry mut DutyRoleEntry,
    nickname: &str,
) -> Option<&'entry mut DutyAgentCard> {
    entry
        .agents
        .iter_mut()
        .find(|card| card.nickname == nickname)
}

fn report_event(
    entry: &mut DutyRoleEntry,
    timestamp: &str,
    nickname: &str,
    report: ScoutReport,
) -> DutyFeedEvent {
    match report {
        ScoutReport::Filed {
            number,
            title,
            evidence,
        } => {
            entry.issues_filed += 1;
            DutyFeedEvent::new(
                timestamp,
                nickname,
                DutyEventKind::File,
                velocity_github::filed_message(number, &title, &evidence),
            )
        }
        ScoutReport::Duplicate { number, title } => DutyFeedEvent::new(
            timestamp,
            nickname,
            DutyEventKind::Skip,
            format!("Skipped a finding · duplicate of #{number} {title}"),
        ),
    }
}

fn text(message: &str) -> AgentInput {
    AgentInput::Send(Vec::from([ContentBlock::from(message.to_owned())]))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
