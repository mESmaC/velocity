//! Running the Main watcher (SPEC.md F7 Main watcher): whenever the base branch's tip on the
//! remote moves, one idle watcher runs one agent turn in its `<watcher>-sweep` worktree at the
//! new tip. It runs every test and stress script, bisects a break to its merge, records the
//! `broken` event and opens a fix or revert PR. Its card follows the agent's steps and its
//! verdicts land in the activity feed. Pausing lets a running watch finish and starts none.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent_client_protocol::v1::schema::ContentBlock;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use velocity_acp::client::{AgentEvent, AgentInput, AircLaunch};
use velocity_core::duty::{DutyAgentCard, DutyEventKind, DutyFeedEvent, DutyRoleEntry};
use velocity_core::main_watch::{
    WatchBrief, WatchReason, WatchSchedule, WatchVerdict, latest_watch, verdict_message, watch_due,
    watch_finished_message, watch_idle_doing, watch_prompt, watch_started_message,
};
use velocity_core::scout_sweep::{SweepRecord, step_doing};
use velocity_github::{ScoutReport, WatchReport, read_scout_reports, read_watch_reports};
use velocity_store::duty::WatchRoster;

use crate::RunningSession;
use crate::sweep::{SweepPlan, sweep_doing};
use crate::watch::{WatchReports, plan_watch, watch_session};

/// How often the runner looks for a new tip when nothing wakes it sooner.
const TICK: Duration = Duration::from_secs(20);
/// How often the runner asks the remote for the base branch's tip.
const BASE_POLL: Duration = Duration::from_secs(60);
/// A card that says this had a watch that failed; it keeps saying so until the next one.
const FAILED: &str = "watch failed";

/// A change to a project's Main watcher: given the feed's timestamp, it returns the events
/// to post.
pub type WatchChange = Box<dyn FnOnce(&mut DutyRoleEntry, &str) -> Vec<DutyFeedEvent> + Send>;

/// Where the runner saves and shows what watches do: the store, the window and its events.
pub trait WatchHost: Clone + Send + Sync + 'static {
    /// Every project's Main watcher role.
    fn rosters(&self) -> Vec<WatchRoster>;
    /// Applies `change` to the project's Main watcher, appends and publishes the events it
    /// returns, saves, and refreshes the window. `false` when the project or the role is gone.
    fn update(&self, project: u32, change: WatchChange) -> bool;
}

type WatcherKey = (u32, String);

/// A running watch: its input, how many of your instructions it still has to answer, and
/// the tip it watches.
struct Running {
    input: UnboundedSender<AgentInput>,
    pending: usize,
    tip: String,
}

#[derive(Default)]
struct RunnerState {
    running: BTreeMap<WatcherKey, Running>,
    /// Per project, your instruction no running watch has taken yet.
    instructions: BTreeMap<u32, String>,
    /// The base branch's tip per project, and when it was read.
    bases: BTreeMap<u32, (Instant, Option<String>)>,
}

/// The watch runner. Cheap to clone; every clone drives the same watches.
#[derive(Clone)]
pub struct MainWatches {
    state: Arc<Mutex<RunnerState>>,
    wake: Arc<Notify>,
    /// The executable that serves the watch and scout MCP servers (Velocity itself).
    executable: PathBuf,
    /// The app data folder: AIRC channels, where `broken` events are posted.
    app_data: PathBuf,
    /// The folder each watch's report files go in.
    reports: PathBuf,
    /// The project's AIRC channel, passed to every watch's agent (F14).
    airc: Option<AircLaunch>,
}

impl MainWatches {
    pub fn new(executable: PathBuf, app_data: PathBuf, reports: PathBuf) -> Self {
        Self {
            state: Arc::default(),
            wake: Arc::new(Notify::new()),
            executable,
            app_data,
            reports,
            airc: None,
        }
    }

    /// Gives every watch's agent the project's AIRC channel under its watcher nick (F14).
    pub fn with_airc(mut self, airc: AircLaunch) -> Self {
        self.airc = Some(airc);
        self
    }

    /// Looks for a new tip now, e.g. after the watcher was started or resumed.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Delivers your instruction from the Duty view's composer (F7: it instructs every agent
    /// in the role). Running watches get it as their next message; with none running, the
    /// next idle watcher watches the tip again with it.
    pub fn instruct(&self, project: u32, instruction: &str) {
        let mut state = self.lock();
        let mut delivered = false;
        for ((owner, _), running) in state.running.iter_mut() {
            if *owner == project && running.input.send(text(instruction)).is_ok() {
                running.pending += 1;
                delivered = true;
            }
        }
        if !delivered {
            state.instructions.insert(project, instruction.to_owned());
        }
        drop(state);
        self.wake();
    }

    /// Watchers whose watches are running, and the tip each one watches.
    pub fn running(&self) -> BTreeMap<WatcherKey, String> {
        self.lock()
            .running
            .iter()
            .map(|(key, running)| (key.clone(), running.tip.clone()))
            .collect()
    }

    /// Runs until the app exits.
    pub async fn run<Host: WatchHost>(self, host: Host) {
        loop {
            self.tick(&host).await;
            // Woken early or timed out: either way, look again.
            let _ = tokio::time::timeout(TICK, self.wake.notified()).await;
        }
    }

    /// Starts a watch for each project whose tip moved, and settles idle cards.
    pub async fn tick<Host: WatchHost>(&self, host: &Host) {
        for roster in host.rosters() {
            let idle: Vec<usize> = {
                let state = self.lock();
                (0..roster.watchers.len())
                    .filter(|index| {
                        let nickname = roster.watchers[*index].nickname.clone();
                        !state.running.contains_key(&(roster.project.id, nickname))
                    })
                    .collect()
            };
            let mut waiting = idle.clone();
            if let (false, Some(first)) = (roster.paused, idle.first()) {
                let tip = self.base_commit(&roster).await;
                if let Some(start) = self.due(&roster, tip.as_deref(), *first) {
                    waiting.retain(|index| index != first);
                    self.start(host, &roster, start);
                }
            }
            for index in waiting {
                settle_card(host, &roster, index);
            }
        }
    }

    /// The watch the role starts now on watcher `index`, if any.
    fn due(&self, roster: &WatchRoster, tip: Option<&str>, index: usize) -> Option<WatchStart> {
        let project = roster.project.id;
        let running: Vec<String> = self
            .lock()
            .running
            .iter()
            .filter(|((owner, _), _)| *owner == project)
            .map(|(_, running)| running.tip.clone())
            .collect();
        let instruction = self.lock().instructions.get(&project).cloned();
        let schedule = WatchSchedule {
            paused: roster.paused,
            tip,
            last: latest_watch(roster.watchers.iter().filter_map(|w| w.sweep.as_ref())),
            running: &running,
            instructed: instruction.is_some(),
        };
        let (reason, tip) = watch_due(&schedule)?;
        if reason == WatchReason::Instruction {
            self.lock().instructions.remove(&project);
        }
        Some(WatchStart {
            index,
            reason,
            tip,
            instruction,
        })
    }

    /// The remote base branch's tip, read at most once a minute.
    async fn base_commit(&self, roster: &WatchRoster) -> Option<String> {
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

    fn start<Host: WatchHost>(&self, host: &Host, roster: &WatchRoster, start: WatchStart) {
        let Some(watcher) = roster.watchers.get(start.index) else {
            return;
        };
        let key = (roster.project.id, watcher.nickname.clone());
        let (input, inputs) = unbounded_channel();
        let running = Running {
            input: input.clone(),
            pending: 0,
            tip: start.tip.clone(),
        };
        self.lock().running.insert(key.clone(), running);
        let watch = Watch {
            runner: self.clone(),
            key,
            roster: roster.clone(),
            start,
            input,
        };
        let host = host.clone();
        tokio::spawn(async move {
            let key = watch.key.clone();
            let runner = watch.runner.clone();
            watch.run(&host, inputs).await;
            runner.lock().running.remove(&key);
            runner.wake();
        });
    }

    fn lock(&self) -> MutexGuard<'_, RunnerState> {
        // The state holds plain maps; a panic elsewhere leaves them usable.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Why and where one watch starts.
struct WatchStart {
    index: usize,
    reason: WatchReason,
    tip: String,
    instruction: Option<String>,
}

/// One watch, from its worktree to its last verdict.
struct Watch {
    runner: MainWatches,
    key: WatcherKey,
    roster: WatchRoster,
    start: WatchStart,
    input: UnboundedSender<AgentInput>,
}

/// What a watch has recorded so far, and how far into each report file it has read.
#[derive(Default)]
struct Progress {
    verdicts: usize,
    verdict_offset: u64,
    issue_offset: u64,
}

impl Watch {
    async fn run<Host: WatchHost>(self, host: &Host, inputs: UnboundedReceiver<AgentInput>) {
        set_card(host, &self.key, true, "preparing the watch worktree".into());
        let (plan, worktree) = match self.prepare().await {
            Ok(prepared) => prepared,
            Err(error) => return self.finish(host, false, Some(&error), 0),
        };
        let reports = self.report_files();
        // Reports left by an earlier run of this watcher would be read as new verdicts.
        let _ = std::fs::remove_file(&reports.verdicts);
        let _ = std::fs::remove_file(&reports.issues);
        if !self.post_started(host, &plan, &worktree) {
            return;
        }
        let mut setup = watch_session(
            &plan,
            worktree,
            &self.runner.executable,
            &self.runner.app_data,
            &self.start.tip,
            &reports,
        );
        setup.airc = self
            .runner
            .airc
            .as_ref()
            .map(|airc| airc.duty(plan.project, &plan.nickname));
        let _ = self.input.send(text(&self.prompt(&plan)));
        let session = crate::spawn_session(plan.agent.clone(), setup, inputs);
        let (verdicts, failure) = self
            .follow(host, session, &reports, &plan.base_branch)
            .await;
        let _ = std::fs::remove_file(&reports.verdicts);
        let _ = std::fs::remove_file(&reports.issues);
        self.finish(host, true, failure.as_deref(), verdicts);
    }

    /// Makes or resets the watch worktree at the tip this watch was given (blocking git work).
    async fn prepare(&self) -> Result<(SweepPlan, PathBuf), String> {
        let roster = self.roster.clone();
        let index = self.start.index;
        let tip = self.start.tip.clone();
        tokio::task::spawn_blocking(move || {
            let watcher = roster.watchers.get(index).ok_or("The watcher left")?;
            let plan = plan_watch(&roster, watcher, &tip).map_err(|error| error.to_string())?;
            let worktree = velocity_git::prepare_sweep_worktree(&plan.worktree)
                .map_err(|error| error.to_string())?;
            Ok((plan, worktree))
        })
        .await
        .map_err(|error| error.to_string())?
    }

    fn report_files(&self) -> WatchReports {
        let stem = format!("{}-{}", self.key.0, self.key.1);
        WatchReports {
            verdicts: self.runner.reports.join(format!("{stem}.jsonl")),
            issues: self.runner.reports.join(format!("{stem}-issues.jsonl")),
        }
    }

    /// Records the watch on the watcher's card and posts its first feed line.
    fn post_started<Host: WatchHost>(
        &self,
        host: &Host,
        plan: &SweepPlan,
        worktree: &Path,
    ) -> bool {
        let record = SweepRecord::new(
            unix_now(),
            Some(self.start.tip.clone()),
            worktree.display().to_string(),
        );
        let message = watch_started_message(&self.start.reason, &plan.base_branch, &self.start.tip);
        let nickname = self.key.1.clone();
        host.update(
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
        )
    }

    fn prompt(&self, plan: &SweepPlan) -> String {
        watch_prompt(&WatchBrief {
            nickname: &self.key.1,
            reason: &self.start.reason,
            base_branch: &plan.base_branch,
            tip: &self.start.tip,
            instruction: self.start.instruction.as_deref(),
        })
    }

    /// Follows the session to the end of its last turn: the card shows each step, and each
    /// report line becomes a feed event. Returns how many verdicts it recorded and why it
    /// stopped early.
    async fn follow<Host: WatchHost>(
        &self,
        host: &Host,
        mut session: RunningSession,
        reports: &WatchReports,
        base: &str,
    ) -> (usize, Option<String>) {
        let mut progress = Progress::default();
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
            self.read_reports(host, reports, base, &mut progress);
            if batch
                .iter()
                .any(|event| matches!(event, AgentEvent::TurnEnded(_)))
            {
                self.end_turn();
            }
        }
        self.read_reports(host, reports, base, &mut progress);
        let failure = match session.outcome.await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some("its session ended unexpectedly".into()),
        };
        (progress.verdicts, failure)
    }

    /// A turn ended: the watch is over unless you sent an instruction it has yet to answer.
    fn end_turn(&self) {
        let mut state = self.runner.lock();
        let Some(running) = state.running.get_mut(&self.key) else {
            return;
        };
        if running.pending > 0 {
            running.pending -= 1;
        } else {
            let _ = running.input.send(AgentInput::Stop);
        }
    }

    /// Posts the verdicts and flaky-test filings recorded since the last read.
    fn read_reports<Host: WatchHost>(
        &self,
        host: &Host,
        reports: &WatchReports,
        base: &str,
        progress: &mut Progress,
    ) {
        let verdicts = match read_watch_reports(&reports.verdicts, progress.verdict_offset) {
            Ok((verdicts, next)) => {
                progress.verdict_offset = next;
                verdicts
            }
            Err(_) => Vec::new(),
        };
        let issues = match read_scout_reports(&reports.issues, progress.issue_offset) {
            Ok((issues, next)) => {
                progress.issue_offset = next;
                issues
            }
            Err(_) => Vec::new(),
        };
        if verdicts.is_empty() && issues.is_empty() {
            return;
        }
        progress.verdicts += verdicts.len();
        let nickname = self.key.1.clone();
        let (base, tip) = (base.to_owned(), self.start.tip.clone());
        host.update(
            self.key.0,
            Box::new(move |entry, timestamp| {
                let mut events: Vec<DutyFeedEvent> = verdicts
                    .into_iter()
                    .map(|report| verdict_event(timestamp, &nickname, &base, &tip, report))
                    .collect();
                events.extend(
                    issues
                        .into_iter()
                        .map(|report| issue_event(entry, timestamp, &nickname, report)),
                );
                events
            }),
        );
    }

    /// The card goes idle and waits for the next merge, or says why this watch failed.
    fn finish<Host: WatchHost>(
        &self,
        host: &Host,
        started: bool,
        failure: Option<&str>,
        verdicts: usize,
    ) {
        let nickname = self.key.1.clone();
        let message = watch_finished_message(&self.start.tip, verdicts, failure);
        let failed = failure.map(|failure| step_doing(&format!("{FAILED} · {failure}")));
        let tip = self.start.tip.clone();
        host.update(
            self.key.0,
            Box::new(move |entry, timestamp| {
                let paused = entry.paused;
                let Some(card) = card_mut(entry, &nickname) else {
                    return Vec::new();
                };
                if !started {
                    // The attempt counts as this tip's watch, so a failing setup waits for
                    // the next merge instead of retrying on every tick.
                    let worktree = card.sweep.take().map(|last| last.worktree);
                    card.sweep = Some(SweepRecord::new(
                        unix_now(),
                        Some(tip),
                        worktree.unwrap_or_default(),
                    ));
                }
                card.busy = false;
                card.doing = failed.unwrap_or_else(|| watch_idle_doing(paused));
                message
                    .map(|message| {
                        DutyFeedEvent::new(timestamp, &nickname, DutyEventKind::Sweep, message)
                    })
                    .into_iter()
                    .collect()
            }),
        );
    }
}

fn verdict_event(
    timestamp: &str,
    nickname: &str,
    base: &str,
    tip: &str,
    report: WatchReport,
) -> DutyFeedEvent {
    let (kind, verdict) = match report {
        WatchReport::Green { .. } => (DutyEventKind::Approve, WatchVerdict::Green),
        WatchReport::Broken {
            commit,
            pull_request,
            command,
        } => (
            DutyEventKind::Changes,
            WatchVerdict::Broken {
                commit,
                pull_request,
                command,
            },
        ),
        WatchReport::Repair {
            number,
            revert,
            commit,
        } => (
            DutyEventKind::Claim,
            WatchVerdict::Repair {
                number,
                revert,
                commit,
            },
        ),
    };
    let message = verdict_message(base, tip, &verdict);
    DutyFeedEvent::new(timestamp, nickname, kind, message)
}

/// A flaky test the watcher filed, or skipped as a duplicate, through the scout server.
fn issue_event(
    entry: &mut DutyRoleEntry,
    timestamp: &str,
    nickname: &str,
    report: ScoutReport,
) -> DutyFeedEvent {
    match report {
        ScoutReport::Filed { number, title } => {
            entry.issues_filed += 1;
            DutyFeedEvent::new(
                timestamp,
                nickname,
                DutyEventKind::File,
                format!("Filed #{number} {title} · failing evidence attached"),
            )
        }
        ScoutReport::Duplicate { number, title } => DutyFeedEvent::new(
            timestamp,
            nickname,
            DutyEventKind::Skip,
            format!("Flaky test already filed as #{number} {title}"),
        ),
    }
}

/// An idle watcher's card: not busy, and waiting for the next merge. Also clears a card a
/// closed app left busy mid-watch. A failed watch keeps its reason until the next one.
fn settle_card<Host: WatchHost>(host: &Host, roster: &WatchRoster, index: usize) {
    let Some(watcher) = roster.watchers.get(index) else {
        return;
    };
    let wanted = watch_idle_doing(roster.paused);
    let failed = watcher.doing.starts_with(FAILED) && !roster.paused;
    let first = watcher.sweep.is_none() && !roster.paused && !watcher.busy;
    if (!watcher.busy && watcher.doing == wanted) || failed || first {
        return;
    }
    set_card(
        host,
        &(roster.project.id, watcher.nickname.clone()),
        false,
        wanted,
    );
}

fn set_card<Host: WatchHost>(host: &Host, key: &WatcherKey, busy: bool, doing: String) {
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

fn text(message: &str) -> AgentInput {
    AgentInput::Send(Vec::from([ContentBlock::from(message.to_owned())]))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
