//! Running PR duty (SPEC.md F7 PR duty, Rule 4): each idle PR duty agent claims one open PR
//! that needs work, so two agents never review the same one. A review runs in the PR's
//! `review-pr-<n>` worktree, on the PR merged onto the base branch, until its agent records a
//! decision (an agent that ends a turn undecided is asked to carry on, a bounded number of
//! times); the agent records its decision and merges only through the PR duty MCP server, and
//! the Duty view's cards and activity feed follow it. Pausing lets running reviews finish and
//! claims nothing new.
//!
//! Most reviews are by reading (F7 "Checks before merge"): the agent decides and its session
//! ends. `landing` merges an approved PR head once GitHub's checks pass, `checker` runs
//! the pre-land checks on an approved Rosen change and lands it, and `post_land` runs the
//! full tests on what landed.

mod checker;
mod landing;
mod listing;
mod post_land;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use agent_client_protocol::v1::schema::ContentBlock;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use velocity_acp::client::{AgentEvent, AgentInput, AircLaunch, duty_member};
use velocity_airc::Channel;
use velocity_core::duty::{DutyAgentCard, DutyEventKind, DutyFeedEvent, DutyRoleEntry};
use velocity_core::merge_gate::ReviewMode;
use velocity_core::pr_review::{
    ReviewAssignment, ReviewAttempt, ReviewBrief, ReviewCandidate, ReviewSubject, ReviewWork,
    TurnEndAction, assign_reviews, claim_message, continue_prompt, resting_heads, review_doing,
    review_finished_message, review_idle_doing, review_prompt, short_change, short_commit,
    turn_end_action,
};
use velocity_core::review_tier::TierAssessment;
use velocity_core::rosen_review::{RosenReviewBrief, rosen_review_prompt};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_github::{
    PrDutyReport, PullRequestClient, PullRequestError, PullRequestOperation, PullRequestState,
    ReviewRecord, read_pr_duty_reports,
};
use velocity_store::duty::PrDutyRoster;

use crate::RunningSession;
use crate::proof_checks::{ProofCheckTarget, check_before_review};
use crate::review::{ReviewPlan, ReviewServer, plan_review, review_session};
use crate::sweep::sweep_doing;
use crate::transcript_view::LiveTranscript;

/// How often the runner looks for PRs that need an agent when nothing wakes it sooner.
const TICK: Duration = Duration::from_secs(30);
/// A card that says this had a review that failed; it keeps saying so until the next one.
const FAILED: &str = "review failed";
/// How often running reviews are given their AIRC messages, as build threads are.
const AIRC_DELIVERY: Duration = Duration::from_secs(1);

/// A change to a project's PR duty: given the feed's timestamp, it returns the events to post.
pub type ReviewChange = Box<dyn FnOnce(&mut DutyRoleEntry, &str) -> Vec<DutyFeedEvent> + Send>;

/// Where the runner saves and shows what reviews do: the store, the window and its events.
pub trait ReviewHost: Clone + Send + Sync + 'static {
    /// Every project's PR duty role.
    fn rosters(&self) -> Vec<PrDutyRoster>;
    /// Applies `change` to the project's PR duty, appends and publishes the events it returns,
    /// saves, and refreshes the window. `false` when the project or the role is gone.
    fn update(&self, project: u32, change: ReviewChange) -> bool;
    /// PR duty requested changes on thread `thread`'s Rosen change: the thread goes back to
    /// fixing, and its agent gets `message` (F5). A PR's author reads them on GitHub instead.
    fn changes_requested(&self, project: u32, thread: u32, change: &str, message: &str);
}

type AgentKey = (u32, String);

/// A running review: its input, how many of your instructions it still has to answer, and
/// the PR or change it holds.
struct Running {
    input: UnboundedSender<AgentInput>,
    pending: usize,
    subject: ReviewSubject,
    /// Its agent was dismissed (F7): the session is stopping and the PR is already released.
    dismissed: bool,
    /// The last AIRC line looked at for this review (F14): the channel's tip at its claim,
    /// so an earlier review's messages are never replayed into this one.
    airc_after: u64,
    /// Set once the review prompt is queued, so an AIRC message never becomes the first
    /// prompt, and cleared when the review is told to stop.
    delivering: bool,
    /// A review by reading approved its Rosen change: the review is over but for its
    /// session's last words, so Velocity's checker takes the change without waiting for them.
    approved: bool,
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
    /// Per project, each PR's or change's last review session: its head rests
    /// (`resting_heads`), so a decided head waits for its author and an undecided one is
    /// retried after a delay.
    attempted: BTreeMap<u32, BTreeMap<ReviewSubject, ReviewAttempt>>,
    /// Counts dismissals, so a tick that read its rosters before one starts no review for the
    /// dismissed agent.
    dismissals: u64,
    /// Each agent's latest review session as the Duty view shows it (F7). Kept after the
    /// session ends so its last review can still be read; a new review replaces it.
    transcripts: BTreeMap<AgentKey, LiveTranscript>,
    /// Per project and PR, what Velocity last saw of the approved head it is landing.
    landing: BTreeMap<(u32, u32), landing::LandingNote>,
    /// The projects with a landing pass running, so two never merge the same head.
    landing_projects: BTreeSet<u32>,
    /// Per project and Rosen change, what Velocity last saw of the approved head it checks.
    checking: BTreeMap<(u32, String), checker::CheckNote>,
    /// The projects with a checker pass running: one build at a time in each check folder.
    checking_projects: BTreeSet<u32>,
    /// Per project, what Velocity has said about the landed batch it last tested.
    verifying: BTreeMap<u32, post_land::VerifyNote>,
    /// The projects with a post-land pass running: one at a time in each folder.
    verifying_projects: BTreeSet<u32>,
    /// The projects with a pass running, each with whether it was asked for again
    /// meanwhile: a project is passed once at a time, and none waits for another.
    passing: BTreeMap<u32, bool>,
    /// Per Rosen project, its open PRs as GitHub last listed them.
    listings: BTreeMap<u32, listing::Listing>,
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
    /// Each checkout's GitHub client, kept across ticks: reading the repository's identity is
    /// a GitHub request (#228).
    clients: Arc<Mutex<BTreeMap<PathBuf, Arc<PullRequestClient>>>>,
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
            clients: Arc::default(),
        }
    }

    /// The checkout's cached GitHub client, discovered on first use and again when the
    /// checkout's `origin` changes or the identity is old (#229).
    fn client(&self, repository: &Path) -> Result<Arc<PullRequestClient>, PullRequestError> {
        // Release the lock before reading the checkout's Git config.
        let cached = self.clients().get(repository).cloned();
        if let Some(client) = cached
            && client.is_current(repository)
        {
            return Ok(client);
        }
        let client = Arc::new(PullRequestClient::discover(repository)?);
        self.clients()
            .insert(repository.to_path_buf(), client.clone());
        Ok(client)
    }

    /// Drops the checkout's client after a failed read, so a changed remote or sign-in is
    /// discovered afresh.
    fn forget_client(&self, repository: &Path) {
        self.clients().remove(repository);
    }

    fn clients(&self) -> MutexGuard<'_, BTreeMap<PathBuf, Arc<PullRequestClient>>> {
        self.clients.lock().unwrap_or_else(PoisonError::into_inner)
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

    /// Each running review's agent and the PR or change it holds.
    pub fn running(&self) -> BTreeMap<AgentKey, ReviewSubject> {
        self.lock()
            .running
            .iter()
            .map(|(key, running)| (key.clone(), running.subject.clone()))
            .collect()
    }

    /// What `nickname`'s latest review session showed, oldest first; `None` before its first
    /// review since Velocity started.
    pub fn transcript(&self, project: u32, nickname: &str) -> Option<Vec<TranscriptBlock>> {
        self.lock()
            .transcripts
            .get(&(project, nickname.to_owned()))
            .map(LiveTranscript::blocks)
    }

    /// The PR `nickname`'s running review holds, if it holds one (not a Rosen change).
    pub fn held(&self, project: u32, nickname: &str) -> Option<u32> {
        self.lock()
            .running
            .get(&(project, nickname.to_owned()))
            .filter(|running| !running.dismissed)
            .and_then(|running| running.subject.number())
    }

    /// Dismisses `nickname` (F7): stops its running review's session, which kills the agent's
    /// process tree, and releases the PR it held at once, in AIRC and on GitHub, so another
    /// agent can claim it on the next tick. Call it after the agent left the saved roster.
    pub fn dismiss(&self, project: u32, nickname: &str, repository: &Path) {
        let mut state = self.lock();
        state.dismissals = state.dismissals.wrapping_add(1);
        let subject = state
            .running
            .get_mut(&(project, nickname.to_owned()))
            .filter(|running| !running.dismissed)
            .map(|running| {
                running.dismissed = true;
                let _ = running.input.send(AgentInput::Stop);
                running.subject.clone()
            });
        if let Some(subject) = &subject
            && let Some(attempts) = state.attempted.get_mut(&project)
        {
            // A released PR or change doesn't rest: the next idle agent takes it.
            attempts.remove(subject);
        }
        drop(state);
        // Only a PR has a claim outside Velocity to release.
        if let Some(number) = subject.and_then(|subject| subject.number()) {
            let runner = self.clone();
            let (repository, nickname) = (repository.to_path_buf(), nickname.to_owned());
            // Blocking GitHub calls, and the window's command may hold no async runtime.
            std::thread::spawn(move || runner.release(&repository, project, &nickname, number));
        }
        self.wake();
    }

    /// Lets PR `number` go for a dismissed agent: the AIRC `released` event, the `reviewing`
    /// label an agent following AGENTS.md may have added, and a comment saying so.
    fn release(&self, repository: &Path, project: u32, nickname: &str, number: u32) {
        if let Ok(record) = ReviewRecord::open(&self.app_data, &project.to_string(), nickname) {
            let _ = record.release(number);
        }
        let Ok(client) = self.client(repository) else {
            return;
        };
        let _ = client.remove_label(number, "reviewing");
        let comment = PullRequestOperation::Comment {
            number,
            body: format!("Released · {nickname} was dismissed from PR duty"),
        };
        let _ = client.execute(comment);
    }

    fn dismissed(&self, key: &AgentKey) -> bool {
        self.lock()
            .running
            .get(key)
            .is_some_and(|running| running.dismissed)
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
        if self.airc.is_some() {
            tokio::spawn(self.clone().deliver_airc_forever());
        }
        loop {
            self.start_passes(&host);
            // Woken early or timed out: either way, look again.
            let _ = tokio::time::timeout(TICK, self.wake.notified()).await;
        }
    }

    /// Starts a pass for every project that has none running. Each project's pass runs on
    /// its own, so a project waiting on GitHub holds up no other; one asked for while its
    /// pass still runs is passed again as soon as that ends.
    fn start_passes<Host: ReviewHost>(&self, host: &Host) {
        let dismissals = self.lock().dismissals;
        for roster in host.rosters() {
            let project = roster.project.id;
            {
                let mut state = self.lock();
                if let Some(again) = state.passing.get_mut(&project) {
                    *again = true;
                    continue;
                }
                state.passing.insert(project, false);
            }
            let (runner, host) = (self.clone(), host.clone());
            tokio::spawn(async move {
                runner.pass(&host, roster, dismissals).await;
                if runner.lock().passing.remove(&project) == Some(true) {
                    runner.wake();
                }
            });
        }
    }

    /// One pass over every project, in turn: for a caller that drives the runner itself.
    pub async fn tick<Host: ReviewHost>(&self, host: &Host) {
        let dismissals = self.lock().dismissals;
        for roster in host.rosters() {
            self.pass(host, roster, dismissals).await;
        }
    }

    /// Claims a PR or change for every idle agent of one project that has one to take,
    /// lands and checks its approved heads, and settles idle cards.
    async fn pass<Host: ReviewHost>(&self, host: &Host, roster: PrDutyRoster, dismissals: u64) {
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
        if roster.paused || roster.agents.is_empty() {
            return settle_cards(host, &roster, &idle, None);
        }
        self.verify_landed(host, &roster);
        // Listed even when every agent is busy: approved heads land without one.
        let candidates = self.candidates(&roster).await;
        if self.lock().dismissals != dismissals {
            // An agent left while PRs were listed: the dismissal's wake reads afresh.
            return;
        }
        match candidates {
            Ok(candidates) => {
                let candidates = self.land_approved(host, &roster, candidates);
                let candidates = self.check_approved(host, &roster, candidates);
                self.claim_for_idle(host, &roster, &candidates, &idle);
            }
            Err(error) => settle_cards(host, &roster, &idle, Some(&error)),
        }
    }

    /// Gives each idle agent a PR that needs one, and settles the cards of the rest.
    fn claim_for_idle<Host: ReviewHost>(
        &self,
        host: &Host,
        roster: &PrDutyRoster,
        candidates: &[ReviewCandidate],
        idle: &[String],
    ) {
        let assignments = self.assign(roster, candidates, idle);
        let claimed: BTreeSet<&str> = assignments.iter().map(|next| next.agent.as_str()).collect();
        let waiting: Vec<String> = idle
            .iter()
            .filter(|nickname| !claimed.contains(nickname.as_str()))
            .cloned()
            .collect();
        settle_cards(host, roster, &waiting, None);
        for assignment in assignments {
            self.start(host, roster, candidates, assignment);
        }
    }

    /// The project's open PRs, each with where the approval record leaves its head, then its
    /// Rosen changes in review.
    ///
    /// A Rosen project's work is in Rosen, so its pass never waits for GitHub: it takes the
    /// PRs as last listed, which `listing` reads again off the pass, and none when they
    /// can't be listed, since a Rosen project needs no GitHub remote. A git project's work
    /// is its PRs, so its pass lists them first.
    async fn candidates(&self, roster: &PrDutyRoster) -> Result<Vec<ReviewCandidate>, String> {
        let rosen = roster.project.version_control == velocity_store::VersionControl::Rosen;
        let mut candidates = if rosen {
            self.listed_pull_requests(roster)
        } else {
            self.pull_request_candidates(roster).await?
        };
        if rosen
            || roster
                .project
                .threads
                .iter()
                .any(|thread| thread.rosen_change.is_some())
        {
            // A failed change lookup still leaves the PRs to review and land.
            match crate::rosen_review::change_candidates(&self.app_data, roster).await {
                Ok(changes) => candidates.extend(changes),
                Err(error) if candidates.is_empty() => return Err(error),
                Err(_) => {}
            }
        }
        Ok(candidates)
    }

    async fn pull_request_candidates(
        &self,
        roster: &PrDutyRoster,
    ) -> Result<Vec<ReviewCandidate>, String> {
        let repository = PathBuf::from(&roster.project.repository_path);
        let app_data = self.app_data.clone();
        let project = roster.project.id.to_string();
        let runner = self.clone();
        tokio::task::spawn_blocking(move || {
            let listed = runner.client(&repository).and_then(|client| client.list());
            if listed.is_err() {
                runner.forget_client(&repository);
            }
            let pull_requests: Vec<_> = listed
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
                    subject: ReviewSubject::PullRequest(pull_request.number),
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
        let held: BTreeSet<ReviewSubject> = state
            .running
            .iter()
            .filter(|((owner, _), running)| *owner == project && !running.dismissed)
            .map(|(_, running)| running.subject.clone())
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
            .find(|candidate| candidate.subject == assignment.subject)
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
                    subject: candidate.subject.clone(),
                    dismissed: false,
                    airc_after: 0,
                    delivering: false,
                    approved: false,
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
            let pull_request = review.candidate.subject.number().is_some();
            review.run(&host, inputs).await;
            {
                let mut state = runner.lock();
                state.running.remove(&key);
                if pull_request {
                    // The review changed where the PR stands: its listing is read again.
                    state.listings.remove(&key.0);
                }
            }
            runner.wake();
        });
    }

    /// Passes each running review the AIRC messages meant for its agent (F14), checked once
    /// a second like build threads. Without this a duty agent never hears a question (#304).
    async fn deliver_airc_forever(self) {
        loop {
            let runner = self.clone();
            let _ = tokio::task::spawn_blocking(move || runner.deliver_airc()).await;
            tokio::time::sleep(AIRC_DELIVERY).await;
        }
    }

    fn deliver_airc(&self) {
        let reviews: Vec<(AgentKey, ReviewSubject, u64)> = self
            .lock()
            .running
            .iter()
            .filter(|(_, running)| running.delivering && !running.dismissed)
            .map(|(key, running)| (key.clone(), running.subject.clone(), running.airc_after))
            .collect();
        for (key, subject, after) in reviews {
            let Some(cursor) = self.deliver_to(&key, &subject, after) else {
                continue;
            };
            if let Some(running) = self.lock().running.get_mut(&key)
                && running.subject == subject
            {
                running.airc_after = running.airc_after.max(cursor);
            }
        }
    }

    /// Delivers targeted messages after `after` to the review of `subject`: each is queued
    /// as the session's next prompt and answered in its own turn. Returns the new cursor.
    fn deliver_to(&self, key: &AgentKey, subject: &ReviewSubject, after: u64) -> Option<u64> {
        let channel = Channel::open(&self.app_data, &key.0.to_string()).ok()?;
        let member = duty_member(&key.1);
        // Only the review's own AIRC server joins: a join made here would outlive its session.
        let nick = channel
            .presence()
            .ok()?
            .into_iter()
            .find(|presence| presence.member == member && presence.joined)?
            .nick;
        let deliver = |_: &str, message: &str| -> Result<bool, String> {
            let mut state = self.lock();
            let Some(running) = state
                .running
                .get_mut(key)
                .filter(|running| running.subject == *subject && running.delivering)
            else {
                return Ok(false);
            };
            if running.input.send(text(message)).is_err() {
                return Ok(false);
            }
            running.pending += 1;
            Ok(true)
        };
        velocity_airc::deliver_targeted(&channel, &nick, after, false, deliver).ok()
    }

    /// Starts a review's AIRC cursor at the channel's tip, as its claim is posted.
    fn start_airc_cursor(&self, key: &AgentKey) {
        let tip = Channel::open(&self.app_data, &key.0.to_string())
            .and_then(|channel| channel.read(0))
            .ok()
            .and_then(|entries| entries.last().map(|entry| entry.sequence))
            .unwrap_or(0);
        if let Some(running) = self.lock().running.get_mut(key) {
            running.airc_after = tip;
        }
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
    /// The review is of a Rosen change by reading: its approval is its decision, since
    /// Velocity's checker lands the approved head.
    approval_decides: bool,
}

/// A prepared review: its plan, its worktree and the head it checks.
struct Prepared {
    plan: ReviewPlan,
    /// The PR's review worktree, or the change's review folder.
    worktree: PathBuf,
    head: String,
    base_commit: String,
    conflicts: Vec<String>,
    tier: TierAssessment,
    mode: ReviewMode,
    /// A Rosen change's MCP server launch file; `None` for a PR.
    launch_file: Option<PathBuf>,
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
        let subject = self.candidate.subject.clone();
        set_card(host, &self.key, true, review_doing(&subject, "claiming"));
        if !self.claim(host).await || self.runner.dismissed(&self.key) {
            return self.finish(host, Outcome::default()).await;
        }
        let step = match subject {
            ReviewSubject::PullRequest(_) => "preparing the review worktree",
            ReviewSubject::Change { .. } => "checking out the merge result",
        };
        set_card(host, &self.key, true, review_doing(&subject, step));
        let prepared = match &subject {
            ReviewSubject::PullRequest(number) => self.prepare(*number).await,
            ReviewSubject::Change { thread, change } => self.prepare_change(*thread, change).await,
        };
        let outcome = match prepared {
            Ok(prepared) => {
                // A change's review folder is removed when its review ends: no card keeps it.
                let proofs = match subject.number() {
                    Some(number) => {
                        keep_worktree(host, &self.key, &prepared.worktree, true);
                        let step = review_doing(&subject, "checking proofs");
                        set_card(host, &self.key, true, step);
                        self.check_proofs(number, &prepared).await
                    }
                    None => String::new(),
                };
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
        let runner = self.runner.clone();
        let project = self.roster.project.id.to_string();
        let (nickname, number, head) = (
            self.key.1.clone(),
            self.candidate.subject.number(),
            self.candidate.head.clone(),
        );
        // Best effort: the runner's own lock is what keeps two agents off one PR, and a
        // missing comment only hides the claimer on the rail. A Rosen change has neither.
        let key = self.key.clone();
        let _ = tokio::task::spawn_blocking(move || {
            runner.start_airc_cursor(&key);
            let Some(number) = number else {
                return;
            };
            if let Ok(record) = ReviewRecord::open(&app_data, &project, &nickname) {
                let _ = record.claim(number, &head);
            }
            let comment = PullRequestOperation::Comment {
                number,
                body: format!("Reviewing · {nickname}"),
            };
            let _ = runner
                .client(&repository)
                .and_then(|client| client.execute(comment));
        })
        .await;
        let message = claim_message(&self.candidate.subject, &self.candidate.title, self.work);
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
    async fn prepare(&self, number: u32) -> Result<Prepared, String> {
        let roster = self.roster.clone();
        let nickname = self.key.1.clone();
        let (runner, work) = (self.runner.clone(), self.work);
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
            let mode = runner.review_mode(&roster, number, work, &checkout, tier.tier);
            Ok(Prepared {
                mode,
                plan,
                worktree: checkout.path,
                head: checkout.head,
                base_commit: checkout.base_commit,
                conflicts: checkout.conflicts,
                tier,
                launch_file: None,
            })
        })
        .await
        .map_err(|error| error.to_string())?
    }

    /// Writes the Rosen change's merge result into its review folder, with the folder setup.
    async fn prepare_change(&self, thread: u32, change: &str) -> Result<Prepared, String> {
        let report = self.report_path();
        if let Some(folder) = report.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        let prepared = crate::rosen_review::prepare_change(crate::rosen_review::ChangeReview {
            app_data: &self.runner.app_data,
            roster: &self.roster,
            nickname: &self.key.1,
            thread,
            change,
            work: self.work,
            report: &report,
        })
        .await?;
        Ok(Prepared {
            worktree: prepared.plan.worktree.worktree_path.clone(),
            plan: prepared.plan,
            head: prepared.head,
            base_commit: prepared.base,
            conflicts: prepared.conflicts,
            tier: prepared.tier,
            mode: prepared.mode,
            launch_file: Some(prepared.launch_file),
        })
    }

    /// The review's report file, which its MCP server appends decisions to.
    fn report_path(&self) -> PathBuf {
        self.runner
            .app_data
            .join("pr-duty-reports")
            .join(format!("{}-{}.jsonl", self.key.0, self.key.1))
    }

    /// Runs the proof checks and the statement lock on the prepared worktree (F17), before the
    /// agent sees the PR. Returns what its prompt adds.
    async fn check_proofs(&self, number: u32, prepared: &Prepared) -> String {
        let app_data = self.runner.app_data.clone();
        let project = self.roster.project.id.to_string();
        let nickname = self.key.1.clone();
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
        let report = self.report_path();
        if let Some(folder) = report.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        // A report left by an earlier review would be read as new decisions.
        let _ = std::fs::remove_file(&report);
        let server = ReviewServer {
            executable: &self.runner.executable,
            app_data: &self.runner.app_data,
            report: &report,
            reading_base: (prepared.mode == ReviewMode::Reading)
                .then_some(prepared.base_commit.as_str()),
        };
        if self.runner.dismissed(&self.key) {
            // Dismissed while the worktree or the proofs were prepared: no session starts.
            if let Some(launch_file) = &prepared.launch_file {
                let _ = std::fs::remove_file(launch_file);
                crate::rosen_review::remove_folder(prepared.worktree.clone()).await;
            }
            return Outcome::default();
        }
        let mut setup = match &prepared.launch_file {
            Some(launch_file) => crate::review::change_review_session(
                &prepared.plan,
                &self.runner.executable,
                launch_file,
            ),
            None => review_session(&prepared.plan, prepared.worktree.clone(), server),
        };
        setup.airc = self
            .runner
            .airc
            .as_ref()
            .map(|airc| airc.duty(prepared.plan.project, &prepared.plan.nickname));
        let _ = self
            .input
            .send(text(&format!("{}{proofs}", self.prompt(&prepared))));
        {
            let mut state = self.runner.lock();
            if let Some(running) = state.running.get_mut(&self.key) {
                running.delivering = true;
            }
            let worktree = prepared.worktree.display().to_string();
            state.transcripts.insert(
                self.key.clone(),
                LiveTranscript::restore(&worktree, Vec::new()),
            );
        }
        let session = crate::spawn_session(prepared.plan.agent.clone(), setup, inputs);
        let reading_change = prepared.launch_file.is_some() && prepared.mode == ReviewMode::Reading;
        let outcome = self.follow(host, session, &report, reading_change).await;
        // Recorded while the claim is still held, so no tick can claim the head in between.
        self.remember(&prepared.head, outcome.decided);
        let _ = std::fs::remove_file(&report);
        if let Some(launch_file) = &prepared.launch_file {
            let _ = std::fs::remove_file(launch_file);
            crate::rosen_review::remove_folder(prepared.worktree.clone()).await;
        } else if outcome.merged {
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
        if self.runner.dismissed(&self.key) {
            return;
        }
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
            .insert(self.candidate.subject.clone(), attempt);
    }

    fn prompt(&self, prepared: &Prepared) -> String {
        let number = match &self.candidate.subject {
            ReviewSubject::PullRequest(number) => *number,
            ReviewSubject::Change { change, .. } => {
                return rosen_review_prompt(&RosenReviewBrief {
                    nickname: &self.key.1,
                    change,
                    title: &self.candidate.title,
                    head: &prepared.head,
                    base_branch: &prepared.plan.base_branch,
                    base_commit: &prepared.base_commit,
                    conflicts: &prepared.conflicts,
                    work: self.work,
                    tier: &prepared.tier,
                    mode: prepared.mode,
                    rules: &self.roster.rules,
                    instruction: self.instruction.as_deref(),
                });
            }
        };
        review_prompt(&ReviewBrief {
            nickname: &self.key.1,
            number,
            title: &self.candidate.title,
            head: &prepared.head,
            head_branch: &self.candidate.head_branch,
            base_branch: &prepared.plan.base_branch,
            base_commit: &prepared.base_commit,
            conflicts: &prepared.conflicts,
            work: self.work,
            tier: &prepared.tier,
            mode: prepared.mode,
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
        approval_decides: bool,
    ) -> Outcome {
        let mut offset = 0;
        let mut outcome = Outcome {
            approval_decides,
            ..Outcome::default()
        };
        let mut doing = String::new();
        let mut nudges = 0;
        while let Some(event) = session.events.recv().await {
            let mut batch = vec![event];
            while let Ok(next) = session.events.try_recv() {
                batch.push(next);
            }
            self.record_transcript(&batch);
            if let Some(step) = batch.iter().rev().find_map(sweep_doing)
                && step != doing
            {
                set_card(
                    host,
                    &self.key,
                    true,
                    review_doing(&self.candidate.subject, &step),
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

    /// A review by reading approved its change: Velocity's checker may take it now, while
    /// the agent's session says its last words and stops.
    fn release_to_checker(&self) {
        if let Some(running) = self.runner.lock().running.get_mut(&self.key) {
            running.approved = true;
        }
        self.runner.wake();
    }

    /// Adds the session's transcript updates to the agent's view of its review.
    fn record_transcript(&self, batch: &[AgentEvent]) {
        let mut state = self.runner.lock();
        let Some(transcript) = state.transcripts.get_mut(&self.key) else {
            return;
        };
        for event in batch {
            if let AgentEvent::Transcript { placement, update } = event {
                transcript.record(*placement, update);
            }
        }
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
                running.delivering = false;
                let _ = running.input.send(AgentInput::Stop);
            }
            TurnEndAction::Nudge => {
                *nudges += 1;
                let subject = &self.candidate.subject;
                let _ = running.input.send(text(&continue_prompt(subject, *nudges)));
                drop(state);
                let step = format!("asked to finish its decision ({nudges})");
                set_card(host, &self.key, true, review_doing(subject, &step));
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
                PrDutyReport::Commented { .. } | PrDutyReport::ChangeCommented { .. } => {}
                // An agent that runs the checks itself also lands: its approval alone
                // leaves the head to be retried. A review by reading ends at its approval.
                PrDutyReport::ChangeApproved { .. } => {
                    outcome.decided |= outcome.approval_decides;
                    if outcome.approval_decides {
                        self.release_to_checker();
                    }
                }
                PrDutyReport::Merged { .. } | PrDutyReport::ChangeLanded { .. } => {
                    outcome.decided = true;
                    outcome.merged = true;
                }
                PrDutyReport::ChangeChangesRequested {
                    change, message, ..
                } => {
                    outcome.decided = true;
                    if let ReviewSubject::Change { thread, .. } = &self.candidate.subject {
                        host.changes_requested(self.key.0, *thread, change, message);
                    }
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
        if self.runner.dismissed(&self.key) {
            // `dismiss` released the PR and posted the feed line; a card under this nick now
            // belongs to a later agent.
            return;
        }
        let app_data = self.runner.app_data.clone();
        let project = self.roster.project.id.to_string();
        let nickname = self.key.1.clone();
        let subject = &self.candidate.subject;
        if let Some(number) = subject.number() {
            let _ = tokio::task::spawn_blocking(move || {
                ReviewRecord::open(&app_data, &project, &nickname)
                    .and_then(|record| record.release(number))
            })
            .await;
        }
        let message = review_finished_message(subject, outcome.decided, outcome.failure.as_deref());
        let failed = outcome
            .failure
            .as_deref()
            .map(|failure| review_doing(subject, &format!("{FAILED} · {failure}")));
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
        PrDutyReport::Held {
            number,
            head,
            reason,
        } => (
            DutyEventKind::Wait,
            velocity_core::scout_sweep::step_doing(&format!(
                "Held #{number} at {} · {reason}",
                short_commit(&head)
            )),
        ),
        PrDutyReport::Commented { .. } | PrDutyReport::ChangeCommented { .. } => return None,
        report => return change_report_event(timestamp, nickname, report),
    };
    Some(DutyFeedEvent::new(timestamp, nickname, kind, message))
}

/// The feed line for a decision on a Rosen change.
fn change_report_event(
    timestamp: &str,
    nickname: &str,
    report: PrDutyReport,
) -> Option<DutyFeedEvent> {
    let (kind, message) = match report {
        PrDutyReport::ChangeApproved { change, head } => (
            DutyEventKind::Approve,
            format!(
                "Approved change {} at {}",
                short_change(&change),
                short_commit(&head)
            ),
        ),
        PrDutyReport::ChangeChangesRequested { change, head, .. } => (
            DutyEventKind::Changes,
            format!(
                "Requested changes on change {} at {}",
                short_change(&change),
                short_commit(&head)
            ),
        ),
        PrDutyReport::ChangeLanded { change, commit } => (
            DutyEventKind::Merge,
            format!(
                "Landed change {} · {}",
                short_change(&change),
                short_commit(&commit)
            ),
        ),
        PrDutyReport::ChangeLandFailed { change, reason } => (
            DutyEventKind::Wait,
            velocity_core::scout_sweep::step_doing(&format!(
                "Land of change {} refused · {reason}",
                short_change(&change)
            )),
        ),
        PrDutyReport::ChangeHeld {
            change,
            head,
            reason,
        } => (
            DutyEventKind::Wait,
            velocity_core::scout_sweep::step_doing(&format!(
                "Held change {} at {} · {reason}",
                short_change(&change),
                short_commit(&head)
            )),
        ),
        _ => return None,
    };
    Some(DutyFeedEvent::new(timestamp, nickname, kind, message))
}

fn text(message: &str) -> AgentInput {
    AgentInput::Send(Vec::from([ContentBlock::from(message.to_owned())]))
}
