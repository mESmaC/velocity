//! Running PR duty (SPEC.md F7 PR duty, Rule 4): each idle PR duty agent claims one open PR
//! that needs work, so two agents never review the same one. A review runs in the PR's
//! `review-pr-<n>` worktree, on the PR merged onto the base branch, until its agent records a
//! decision (an agent that ends a turn undecided is asked to carry on, a bounded number of
//! times); the agent records its decision and merges only through the PR duty MCP server, and
//! the Duty view's cards and activity feed follow it. Pausing lets running reviews finish and
//! claims nothing new.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use agent_client_protocol::v1::schema::ContentBlock;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use velocity_acp::client::{AgentEvent, AgentInput, AircLaunch};
use velocity_core::duty::{DutyAgentCard, DutyEventKind, DutyFeedEvent, DutyRoleEntry};
use velocity_core::pr_review::{
    ReviewAssignment, ReviewAttempt, ReviewBrief, ReviewCandidate, ReviewWork, TurnEndAction,
    assign_reviews, claim_message, continue_prompt, resting_heads, review_doing,
    review_finished_message, review_idle_doing, review_prompt, short_commit, turn_end_action,
};
use velocity_core::review_tier::TierAssessment;
use velocity_github::{
    PrDutyReport, PullRequestClient, PullRequestOperation, PullRequestState, ReviewRecord,
    read_pr_duty_reports,
};
use velocity_store::duty::PrDutyRoster;

use crate::RunningSession;
use crate::proof_checks::{ProofCheckTarget, check_before_review};
use crate::review::{ReviewPlan, ReviewServer, plan_review, review_session};
use crate::sweep::sweep_doing;

/// How often the runner looks for PRs that need an agent when nothing wakes it sooner.
const TICK: Duration = Duration::from_secs(30);
/// A card that says this had a review that failed; it keeps saying so until the next one.
const FAILED: &str = "review failed";

/// A change to a project's PR duty: given the feed's timestamp, it returns the events to post.
pub type ReviewChange = Box<dyn FnOnce(&mut DutyRoleEntry, &str) -> Vec<DutyFeedEvent> + Send>;

/// Where the runner saves and shows what reviews do: the store, the window and its events.
pub trait ReviewHost: Clone + Send + Sync + 'static {
    /// Every project's PR duty role.
    fn rosters(&self) -> Vec<PrDutyRoster>;
    /// Applies `change` to the project's PR duty, appends and publishes the events it returns,
    /// saves, and refreshes the window. `false` when the project or the role is gone.
    fn update(&self, project: u32, change: ReviewChange) -> bool;
}

type AgentKey = (u32, String);

/// A running review: its input, how many of your instructions it still has to answer, and
/// the PR it holds.
struct Running {
    input: UnboundedSender<AgentInput>,
    pending: usize,
    number: u32,
}

/// Your instruction for a project's PR duty, and the agents that already have it.
struct Instruction {
    text: String,
    delivered: BTreeSet<String>,
}

#[derive(Default)]
struct RunnerState {
    running: BTreeMap<AgentKey, Running>,
    instructions: BTreeMap<u32, Instruction>,
    /// Per project, each PR's last review session: its head rests (`resting_heads`), so a
    /// decided head waits for its author and an undecided one is retried after a delay.
    attempted: BTreeMap<u32, BTreeMap<u32, ReviewAttempt>>,
}

/// The review runner. Cheap to clone; every clone drives the same reviews.
#[derive(Clone)]
pub struct PrDutyReviews {
    state: Arc<Mutex<RunnerState>>,
    wake: Arc<Notify>,
    /// The executable that serves the PR duty MCP server (Velocity itself).
    executable: PathBuf,
    /// The app data folder: AIRC channels (the approval record) and review reports.
    app_data: PathBuf,
    /// The project's AIRC channel, passed to every review's agent (F14).
    airc: Option<AircLaunch>,
    /// The runner's clock: when each attempt ended, so an undecided head's rest can end.
    epoch: Instant,
}

impl PrDutyReviews {
    pub fn new(executable: PathBuf, app_data: PathBuf) -> Self {
        Self {
            epoch: Instant::now(),
            state: Arc::default(),
            wake: Arc::new(Notify::new()),
            executable,
            app_data,
            airc: None,
        }
    }

    /// Gives every review's agent the project's AIRC channel under its PR duty nick (F14).
    pub fn with_airc(mut self, airc: AircLaunch) -> Self {
        self.airc = Some(airc);
        self
    }

    /// Looks for PRs to claim now, e.g. after PR duty was started or resumed.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Each running review's agent and the PR it holds.
    pub fn running(&self) -> BTreeMap<AgentKey, u32> {
        self.lock()
            .running
            .iter()
            .map(|(key, running)| (key.clone(), running.number))
            .collect()
    }

    /// Delivers your instruction from the Duty view's composer (F7: it instructs every agent
    /// in the role). A running review gets it as its next message; each idle agent gets it with
    /// its next claim. PRs already looked at are looked at again.
    pub fn instruct(&self, project: u32, instruction: &str) {
        let mut state = self.lock();
        let mut delivered = BTreeSet::new();
        for ((owner, nickname), running) in state.running.iter_mut() {
            if *owner == project && running.input.send(text(instruction)).is_ok() {
                running.pending += 1;
                delivered.insert(nickname.clone());
            }
        }
        let instruction = Instruction {
            text: instruction.to_owned(),
            delivered,
        };
        state.instructions.insert(project, instruction);
        state.attempted.remove(&project);
        drop(state);
        self.wake();
    }

    /// Runs until the app exits.
    pub async fn run<Host: ReviewHost>(self, host: Host) {
        loop {
            self.tick(&host).await;
            // Woken early or timed out: either way, look again.
            let _ = tokio::time::timeout(TICK, self.wake.notified()).await;
        }
    }

    /// Claims a PR for every idle agent that has one to take, and settles idle cards.
    pub async fn tick<Host: ReviewHost>(&self, host: &Host) {
        for roster in host.rosters() {
            let idle: Vec<String> = {
                let state = self.lock();
                roster
                    .agents
                    .iter()
                    .map(|agent| agent.nickname.clone())
                    .filter(|nickname| {
                        !state
                            .running
                            .contains_key(&(roster.project.id, nickname.clone()))
                    })
                    .collect()
            };
            if roster.paused || idle.is_empty() {
                settle_cards(host, &roster, &idle, None);
                continue;
            }
            match self.candidates(&roster).await {
                Ok(candidates) => {
                    let assignments = self.assign(&roster, &candidates, &idle);
                    let claimed: BTreeSet<&str> =
                        assignments.iter().map(|next| next.agent.as_str()).collect();
                    let waiting: Vec<String> = idle
                        .iter()
                        .filter(|nickname| !claimed.contains(nickname.as_str()))
                        .cloned()
                        .collect();
                    settle_cards(host, &roster, &waiting, None);
                    for assignment in assignments {
                        self.start(host, &roster, &candidates, assignment);
                    }
                }
                Err(error) => settle_cards(host, &roster, &idle, Some(&error)),
            }
        }
    }

    /// The project's open PRs, each with where the approval record leaves its head.
    async fn candidates(&self, roster: &PrDutyRoster) -> Result<Vec<ReviewCandidate>, String> {
        let repository = PathBuf::from(&roster.project.repository_path);
        let app_data = self.app_data.clone();
        let project = roster.project.id.to_string();
        tokio::task::spawn_blocking(move || {
            let pull_requests: Vec<_> = PullRequestClient::discover(&repository)
                .and_then(|client| client.list())
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|pull_request| pull_request.state == PullRequestState::Open)
                .collect();
            let heads: Vec<(u32, &str)> = pull_requests
                .iter()
                .map(|pull_request| (pull_request.number, pull_request.head_ref_oid.as_str()))
                .collect();
            let standings = ReviewRecord::open(&app_data, &project, "pr-duty")
                .and_then(|record| record.standings(&heads))
                .map_err(|error| error.to_string())?;
            Ok(pull_requests
                .into_iter()
                .zip(standings)
                .map(|(pull_request, standing)| ReviewCandidate {
                    number: pull_request.number,
                    title: pull_request.title,
                    head: pull_request.head_ref_oid,
                    head_branch: pull_request.head_ref_name,
                    draft: pull_request.is_draft,
                    standing,
                })
                .collect())
        })
        .await
        .map_err(|error| error.to_string())?
    }

    fn assign(
        &self,
        roster: &PrDutyRoster,
        candidates: &[ReviewCandidate],
        idle: &[String],
    ) -> Vec<ReviewAssignment> {
        let state = self.lock();
        let project = roster.project.id;
        let held: BTreeSet<u32> = state
            .running
            .iter()
            .filter(|((owner, _), _)| *owner == project)
            .map(|(_, running)| running.number)
            .collect();
        let resting = state
            .attempted
            .get(&project)
            .map(|attempts| resting_heads(attempts, self.epoch.elapsed()))
            .unwrap_or_default();
        assign_reviews(candidates, idle, &held, &resting)
    }

    fn start<Host: ReviewHost>(
        &self,
        host: &Host,
        roster: &PrDutyRoster,
        candidates: &[ReviewCandidate],
        assignment: ReviewAssignment,
    ) {
        let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.number == assignment.number)
        else {
            return;
        };
        let key = (roster.project.id, assignment.agent.clone());
        let (input, inputs) = unbounded_channel();
        let instruction = {
            let mut state = self.lock();
            state.running.insert(
                key.clone(),
                Running {
                    input: input.clone(),
                    pending: 0,
                    number: candidate.number,
                },
            );
            state
                .instructions
                .get_mut(&roster.project.id)
                .and_then(|wanted| {
                    let first = wanted.delivered.insert(assignment.agent.clone());
                    first.then(|| wanted.text.clone())
                })
        };
        let review = Review {
            runner: self.clone(),
            key,
            roster: roster.clone(),
            candidate: candidate.clone(),
            work: assignment.work,
            instruction,
            input,
        };
        let host = host.clone();
        tokio::spawn(async move {
            let key = review.key.clone();
            let runner = review.runner.clone();
            review.run(&host, inputs).await;
            runner.lock().running.remove(&key);
            runner.wake();
        });
    }

    fn lock(&self) -> MutexGuard<'_, RunnerState> {
        // The state holds plain maps; a panic elsewhere leaves them usable.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What one review ended with, for its feed line and its worktree.
#[derive(Default)]
struct Outcome {
    decided: bool,
    merged: bool,
    failure: Option<String>,
}

/// A prepared review: its plan, its worktree and the head it checks.
struct Prepared {
    plan: ReviewPlan,
    worktree: PathBuf,
    head: String,
    base_commit: String,
    conflicts: Vec<String>,
    tier: TierAssessment,
}

/// One agent's review of one PR, from its claim to its release.
struct Review {
    runner: PrDutyReviews,
    key: AgentKey,
    roster: PrDutyRoster,
    candidate: ReviewCandidate,
    work: ReviewWork,
    instruction: Option<String>,
    input: UnboundedSender<AgentInput>,
}

impl Review {
    async fn run<Host: ReviewHost>(self, host: &Host, inputs: UnboundedReceiver<AgentInput>) {
        let number = self.candidate.number;
        set_card(host, &self.key, true, review_doing(number, "claiming"));
        if !self.claim(host).await {
            return;
        }
        set_card(
            host,
            &self.key,
            true,
            review_doing(number, "preparing the review worktree"),
        );
        let outcome = match self.prepare().await {
            Ok(prepared) => {
                keep_worktree(host, &self.key, &prepared.worktree, true);
                set_card(
                    host,
                    &self.key,
                    true,
                    review_doing(number, "checking proofs"),
                );
                let proofs = self.check_proofs(&prepared).await;
                self.review(host, prepared, &proofs, inputs).await
            }
            Err(failure) => Outcome {
                failure: Some(failure),
                ..Outcome::default()
            },
        };
        self.finish(host, outcome).await;
    }

    /// Posts the claim in AIRC and on the PR, where the rail reads its claiming agent (F7).
    async fn claim<Host: ReviewHost>(&self, host: &Host) -> bool {
        let repository = PathBuf::from(&self.roster.project.repository_path);
        let app_data = self.runner.app_data.clone();
        let project = self.roster.project.id.to_string();
        let (nickname, number, head) = (
            self.key.1.clone(),
            self.candidate.number,
            self.candidate.head.clone(),
        );
        // Best effort: the runner's own lock is what keeps two agents off one PR, and a
        // missing comment only hides the claimer on the rail.
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(record) = ReviewRecord::open(&app_data, &project, &nickname) {
                let _ = record.claim(number, &head);
            }
            let comment = PullRequestOperation::Comment {
                number,
                body: format!("Reviewing · {nickname}"),
            };
            let _ =
                PullRequestClient::discover(&repository).and_then(|client| client.execute(comment));
        })
        .await;
        let message = claim_message(number, &self.candidate.title, self.work);
        let nickname = self.key.1.clone();
        host.update(
            self.key.0,
            Box::new(move |_, timestamp| {
                vec![DutyFeedEvent::new(
                    timestamp,
                    &nickname,
                    DutyEventKind::Claim,
                    message,
                )]
            }),
        )
    }

    /// Makes or resets the review worktree at the PR head merged onto the base branch's tip
    /// (blocking git work).
    async fn prepare(&self) -> Result<Prepared, String> {
        let roster = self.roster.clone();
        let nickname = self.key.1.clone();
        let number = self.candidate.number;
        tokio::task::spawn_blocking(move || {
            let agent = roster
                .agents
                .iter()
                .find(|agent| agent.nickname == nickname)
                .ok_or("The agent left PR duty")?;
            let repository = PathBuf::from(&roster.project.repository_path);
            let branch = crate::launch::base_branch(&roster.project);
            let (_, base_commit) = velocity_git::sweep_base(&repository, &branch)
                .ok_or_else(|| format!("Could not find the base branch `{branch}`"))?;
            let plan = plan_review(&roster, agent, number, &base_commit)
                .map_err(|error| error.to_string())?;
            let checkout = velocity_git::prepare_review_worktree(&plan.worktree, number)
                .map_err(|error| error.to_string())?;
            let tier =
                velocity_git::review_tier(&checkout.path, &checkout.base_commit, &checkout.head)
                    .map_err(|error| format!("Could not read the PR's diff: {error}"))?;
            Ok(Prepared {
                plan,
                worktree: checkout.path,
                head: checkout.head,
                base_commit: checkout.base_commit,
                conflicts: checkout.conflicts,
                tier,
            })
        })
        .await
        .map_err(|error| error.to_string())?
    }

    /// Runs the proof checks and the statement lock on the prepared worktree (F17), before the
    /// agent sees the PR. Returns what its prompt adds.
    async fn check_proofs(&self, prepared: &Prepared) -> String {
        let app_data = self.runner.app_data.clone();
        let project = self.roster.project.id.to_string();
        let nickname = self.key.1.clone();
        let number = self.candidate.number;
        let worktree = prepared.worktree.clone();
        let (base_commit, head) = (prepared.base_commit.clone(), prepared.head.clone());
        tokio::task::spawn_blocking(move || {
            check_before_review(&ProofCheckTarget {
                app_data: &app_data,
                project: &project,
                nickname: &nickname,
                number,
                worktree: &worktree,
                base_commit: &base_commit,
                head: &head,
            })
        })
        .await
        .unwrap_or_else(|error| format!("\nProof checks stopped unexpectedly ({error}).\n"))
    }

    async fn review<Host: ReviewHost>(
        &self,
        host: &Host,
        prepared: Prepared,
        proofs: &str,
        inputs: UnboundedReceiver<AgentInput>,
    ) -> Outcome {
        let report = self
            .runner
            .app_data
            .join("pr-duty-reports")
            .join(format!("{}-{}.jsonl", self.key.0, self.key.1));
        if let Some(folder) = report.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        // A report left by an earlier review would be read as new decisions.
        let _ = std::fs::remove_file(&report);
        let server = ReviewServer {
            executable: &self.runner.executable,
            app_data: &self.runner.app_data,
            report: &report,
        };
        let mut setup = review_session(&prepared.plan, prepared.worktree.clone(), server);
        setup.airc = self
            .runner
            .airc
            .as_ref()
            .map(|airc| airc.duty(prepared.plan.project, &prepared.plan.nickname));
        let _ = self
            .input
            .send(text(&format!("{}{proofs}", self.prompt(&prepared))));
        let session = crate::spawn_session(prepared.plan.agent.clone(), setup, inputs);
        let outcome = self.follow(host, session, &report).await;
        // Recorded while the claim is still held, so no tick can claim the head in between.
        self.remember(&prepared.head, outcome.decided);
        let _ = std::fs::remove_file(&report);
        if outcome.merged {
            // Rule 5: the review worktree goes as soon as its PR merges.
            let repository = prepared.plan.worktree.repository_root.clone();
            let worktree = prepared.worktree.clone();
            let removed = tokio::task::spawn_blocking(move || {
                velocity_git::remove_review_worktree(&repository, &worktree)
            })
            .await;
            if matches!(removed, Ok(Ok(()))) {
                keep_worktree(host, &self.key, &prepared.worktree, false);
            }
        }
        outcome
    }

    /// The head this session reviewed rests: until it moves if the session decided, and for
    /// `UNDECIDED_RETRY_AFTER` if it did not.
    fn remember(&self, head: &str, decided: bool) {
        let attempt = ReviewAttempt {
            head: head.to_owned(),
            decided,
            ended_at: self.runner.epoch.elapsed(),
        };
        self.runner
            .lock()
            .attempted
            .entry(self.key.0)
            .or_default()
            .insert(self.candidate.number, attempt);
    }

    fn prompt(&self, prepared: &Prepared) -> String {
        review_prompt(&ReviewBrief {
            nickname: &self.key.1,
            number: self.candidate.number,
            title: &self.candidate.title,
            head: &prepared.head,
            head_branch: &self.candidate.head_branch,
            base_branch: &prepared.plan.base_branch,
            base_commit: &prepared.base_commit,
            conflicts: &prepared.conflicts,
            work: self.work,
            tier: &prepared.tier,
            rules: &self.roster.rules,
            instruction: self.instruction.as_deref(),
        })
    }

    /// Follows the session to the end of its last turn: the card shows each step, and each
    /// report line becomes a feed event.
    async fn follow<Host: ReviewHost>(
        &self,
        host: &Host,
        mut session: RunningSession,
        report: &Path,
    ) -> Outcome {
        let mut offset = 0;
        let mut outcome = Outcome::default();
        let mut doing = String::new();
        let mut nudges = 0;
        while let Some(event) = session.events.recv().await {
            let mut batch = vec![event];
            while let Ok(next) = session.events.try_recv() {
                batch.push(next);
            }
            if let Some(step) = batch.iter().rev().find_map(sweep_doing)
                && step != doing
            {
                set_card(
                    host,
                    &self.key,
                    true,
                    review_doing(self.candidate.number, &step),
                );
                doing = step;
            }
            offset = self.read_reports(host, report, offset, &mut outcome);
            if batch
                .iter()
                .any(|event| matches!(event, AgentEvent::TurnEnded(_)))
            {
                self.end_turn(host, outcome.decided, &mut nudges);
            }
        }
        self.read_reports(host, report, offset, &mut outcome);
        outcome.failure = match session.outcome.await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => Some("its session ended unexpectedly".into()),
        };
        outcome
    }

    /// A turn ended: answer your next instruction, ask an undecided agent to carry on, or end
    /// the review (`turn_end_action`).
    fn end_turn<Host: ReviewHost>(&self, host: &Host, decided: bool, nudges: &mut u32) {
        let mut state = self.runner.lock();
        let Some(running) = state.running.get_mut(&self.key) else {
            return;
        };
        match turn_end_action(running.pending, decided, *nudges) {
            TurnEndAction::AnswerInstruction => running.pending -= 1,
            TurnEndAction::Stop => {
                let _ = running.input.send(AgentInput::Stop);
            }
            TurnEndAction::Nudge => {
                *nudges += 1;
                let number = self.candidate.number;
                let _ = running.input.send(text(&continue_prompt(number, *nudges)));
                drop(state);
                let step = format!("asked to finish its decision ({nudges})");
                set_card(host, &self.key, true, review_doing(number, &step));
            }
        }
    }

    fn read_reports<Host: ReviewHost>(
        &self,
        host: &Host,
        report: &Path,
        offset: u64,
        outcome: &mut Outcome,
    ) -> u64 {
        let Ok((reports, next)) = read_pr_duty_reports(report, offset) else {
            return offset;
        };
        if reports.is_empty() {
            return next;
        }
        for report in &reports {
            match report {
                PrDutyReport::Commented { .. } => {}
                PrDutyReport::Merged { .. } => {
                    outcome.decided = true;
                    outcome.merged = true;
                }
                _ => outcome.decided = true,
            }
        }
        let nickname = self.key.1.clone();
        host.update(
            self.key.0,
            Box::new(move |_, timestamp| {
                reports
                    .into_iter()
                    .filter_map(|report| report_event(timestamp, &nickname, report))
                    .collect()
            }),
        );
        next
    }

    /// Releases the claim; the card goes idle, or says why the review failed.
    async fn finish<Host: ReviewHost>(&self, host: &Host, outcome: Outcome) {
        let app_data = self.runner.app_data.clone();
        let project = self.roster.project.id.to_string();
        let (nickname, number) = (self.key.1.clone(), self.candidate.number);
        let _ = tokio::task::spawn_blocking(move || {
            ReviewRecord::open(&app_data, &project, &nickname)
                .and_then(|record| record.release(number))
        })
        .await;
        let message = review_finished_message(number, outcome.decided, outcome.failure.as_deref());
        let failed = outcome
            .failure
            .as_deref()
            .map(|failure| review_doing(number, &format!("{FAILED} · {failure}")));
        let nickname = self.key.1.clone();
        host.update(
            self.key.0,
            Box::new(move |entry, timestamp| {
                let paused = entry.paused;
                if let Some(card) = card_mut(entry, &nickname) {
                    card.busy = false;
                    card.doing = failed.unwrap_or_else(|| review_idle_doing(paused));
                }
                message
                    .map(|message| {
                        DutyFeedEvent::new(timestamp, &nickname, DutyEventKind::Skip, message)
                    })
                    .into_iter()
                    .collect()
            }),
        );
    }
}

/// Idle cards say they are watching for PRs (or paused, or why PRs could not be listed). Also
/// clears a card a closed app left busy mid-review. A failed review's card keeps saying so.
fn settle_cards<Host: ReviewHost>(
    host: &Host,
    roster: &PrDutyRoster,
    idle: &[String],
    error: Option<&str>,
) {
    let wanted = match error {
        Some(error) => velocity_core::scout_sweep::step_doing(&format!("idle · {error}")),
        None => review_idle_doing(roster.paused),
    };
    for agent in roster
        .agents
        .iter()
        .filter(|agent| idle.contains(&agent.nickname))
    {
        let failed = agent.doing.contains(FAILED) && !roster.paused;
        if !agent.busy && (agent.doing == wanted || failed) {
            continue;
        }
        let key = (roster.project.id, agent.nickname.clone());
        set_card(host, &key, false, wanted.clone());
    }
}

/// Records (or forgets) a kept review worktree on the agent's card, so Settings → Worktrees
/// lists it as PR duty's (F5).
fn keep_worktree<Host: ReviewHost>(host: &Host, key: &AgentKey, worktree: &Path, kept: bool) {
    let nickname = key.1.clone();
    let path = worktree.display().to_string();
    host.update(
        key.0,
        Box::new(move |entry, _| {
            if let Some(card) = card_mut(entry, &nickname) {
                card.review_worktrees.retain(|known| *known != path);
                if kept {
                    card.review_worktrees.push(path);
                }
            }
            Vec::new()
        }),
    );
}

fn set_card<Host: ReviewHost>(host: &Host, key: &AgentKey, busy: bool, doing: String) {
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

fn report_event(timestamp: &str, nickname: &str, report: PrDutyReport) -> Option<DutyFeedEvent> {
    let (kind, message) = match report {
        PrDutyReport::Approved { number, head } => (
            DutyEventKind::Approve,
            format!("Approved #{number} at {}", short_commit(&head)),
        ),
        PrDutyReport::ChangesRequested { number, head } => (
            DutyEventKind::Changes,
            format!("Requested changes on #{number} at {}", short_commit(&head)),
        ),
        PrDutyReport::Merged {
            number,
            branch_kept,
        } => {
            let branch = if branch_kept {
                "branch kept"
            } else {
                "branch deleted"
            };
            (DutyEventKind::Merge, format!("Merged #{number} · {branch}"))
        }
        PrDutyReport::MergeFailed { number, reason } => (
            DutyEventKind::Wait,
            velocity_core::scout_sweep::step_doing(&format!(
                "Merge of #{number} refused · {reason}"
            )),
        ),
        PrDutyReport::Commented { .. } => return None,
    };
    Some(DutyFeedEvent::new(timestamp, nickname, kind, message))
}

fn text(message: &str) -> AgentInput {
    AgentInput::Send(Vec::from([ContentBlock::from(message.to_owned())]))
}
