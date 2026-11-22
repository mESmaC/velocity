//! Goal swarm member planning, work choice, claim contention and `keep_going` (`SPEC.md` F16).
//!
//! Each agent starts in its own `<nick>` worktree, enters `choosing work`, reads the work
//! pool and AIRC claims board, claims an open piece with `Channel::claim_piece` (retrying
//! within one step on refusal), renames its git branch for the piece while keeping its
//! `<nick>` worktree folder, or removes its untouched worktree and leaves when nothing is
//! unclaimed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use thiserror::Error;
use velocity_acp::client::AgentLaunch;
use velocity_airc::waits::Checkout;
use velocity_airc::{Channel, ClaimResult, Event};
use velocity_core::goal_swarm::{GoalEventKind, GoalFeedEvent};
use velocity_core::goal_swarm_choice::{
    PostMergeDecision, decide_after_merge, open_flags_for_pool,
};
use velocity_core::work_pool::{WorkKey, WorkPiece, WorkPool};
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_git::{WorktreeError, WorktreeSetupRequest};
use velocity_store::goal_swarms::{CHOOSING_THREAD_TITLE, GoalAgentSlot};
use velocity_store::rosen_work::{IssueVersion, OwnedIssueVersion};
use velocity_store::threads::{NewThread, ThreadError};
use velocity_store::{ProjectItem, Settings, StateStore};

use crate::goal_selection::{Candidate, HeldWork, candidates, needs_choice, shortlist};
use crate::launch::{
    LaunchError, Location, agent_launch, base_branch, setup_settings, with_compiler_cache,
};
use crate::naming::unique_slug;
use crate::work_pool::{WorkPoolError, read_goal_work_pool};
use velocity_graph::coordination::SubsystemMap;

const SHARED_TARGET_FOLDER: &str = "target";

#[derive(Debug, Error)]
pub enum GoalSwarmError {
    #[error("{0}")]
    LockPoisoned(String),
    #[error("Could not choose goal swarm work: {0}")]
    Selection(String),
    #[error(transparent)]
    Launch(#[from] LaunchError),
    #[error(transparent)]
    Store(#[from] ThreadError),
    #[error(transparent)]
    Pool(#[from] WorkPoolError),
    #[error(transparent)]
    Channel(#[from] velocity_airc::Error),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
}

/// Planned worktree and ACP process launch for one goal swarm slot.
#[derive(Clone)]
pub struct GoalMemberPlan {
    pub project_id: u32,
    pub slot: GoalAgentSlot,
    pub thread: NewThread,
    pub worktree: WorktreeSetupRequest,
    pub agent: AgentLaunch,
    pub bypass_modes: Vec<String>,
}

/// Claimed piece, renamed branch and full prompt ready to start the agent's ACP session.
#[derive(Clone, Debug)]
pub struct ClaimedGoalWork {
    pub piece: WorkPiece,
    pub branch_name: String,
    pub prompt: String,
    pub feed_events: Vec<GoalFeedEvent>,
}

/// Result of running the F16 choosing -> claimed / leave phase for one agent.
#[derive(Clone, Debug)]
pub enum ClaimPhaseOutcome {
    Claimed(Box<ClaimedGoalWork>),
    Left {
        feed_events: Vec<GoalFeedEvent>,
        left_entry: Option<Box<velocity_airc::Entry>>,
    },
}

/// Result of evaluating `keep_going` after a goal swarm member's PR merges (F16 step 5).
#[derive(Clone, Debug)]
pub enum KeepGoingOutcome {
    Stopped,
    LeftEmpty {
        swarm_id: u32,
        feed_event: GoalFeedEvent,
        left_entry: Option<Box<velocity_airc::Entry>>,
    },
    NextRound(GoalAgentSlot),
}

#[cfg(test)]
type BeforeClaimHook<'a> = &'a dyn Fn(&WorkPiece) -> Result<(), velocity_airc::Error>;

struct ClaimContext<'a> {
    store: &'a Mutex<StateStore>,
    channel: &'a Channel,
    member: String,
    nick: String,
    project_id: u32,
    thread_id: u32,
    slot: &'a GoalAgentSlot,
    worktree_path: &'a Path,
    initial_branch: &'a str,
    timestamp: &'a str,
    rosen: bool,
    #[cfg(test)]
    before_claim: Option<BeforeClaimHook<'a>>,
}

/// Serialize the board read and claim between local members when using advisory subsystem
/// classification or Rosen identity-only reservations (where known-file overlaps are checked
/// on the refreshed board rather than reserved as Rosen path claims). The channel itself
/// decides the atomic piece lock (and file locks on legacy git channels).
static SUBSYSTEM_CLAIM_STEP: Mutex<()> = Mutex::new(());

pub trait GoalChoice {
    fn choose(&mut self, shortlist: &[&WorkPiece]) -> Result<Vec<String>, GoalSwarmError>;

    /// The chooser must stop before an empty workspace can be removed.
    fn stop(&mut self) -> Result<(), GoalSwarmError>;
}

/// Plans a `<nick>` worktree (`<root>\<project>\<nick>`, provisional branch `vel/<nick>`)
/// and ACP process for `slot` (`SPEC.md` F16 step 1).
pub fn plan_goal_member(
    project: &ProjectItem,
    worktrees: &WorktreePreferences,
    settings: &Settings,
    slot: &GoalAgentSlot,
) -> Result<GoalMemberPlan, LaunchError> {
    let location = Location::new(project, worktrees);
    let repository = PathBuf::from(&project.repository_path);
    let leaf = unique_slug(&slot.nick, |candidate| {
        nick_taken(project, &location, candidate)
    })
    .ok_or_else(|| LaunchError::NamesExhausted(slot.nick.clone()))?;
    let worktree_path = member_workspace_path(&location, &leaf, &slot.nick)?;
    let shared_target = location.project_root()?.join(SHARED_TARGET_FOLDER);
    let branch_name = format!("vel/{leaf}");
    // A provider you signed out of since the mix was chosen is refused, never swapped (F15).
    settings.ensure_new_work(slot.provider)?;
    let (agent, bypass_modes) = agent_launch(settings, slot.provider)?;
    let mut member_settings = settings.clone();
    member_settings.providers.default_provider = slot.provider;
    member_settings.providers.default_model = Some(slot.model_name.clone());
    member_settings.providers.default_effort = slot.effort;
    member_settings.providers.default_speed = slot.speed;
    let mut thread = NewThread::new(
        CHOOSING_THREAD_TITLE.into(),
        branch_name.clone(),
        worktree_path.display().to_string(),
        &member_settings,
    );
    thread.model_supports_fast = true;
    Ok(GoalMemberPlan {
        project_id: project.id,
        slot: slot.clone(),
        thread,
        worktree: WorktreeSetupRequest::branched(
            repository,
            worktree_path,
            branch_name,
            base_branch(project),
            setup_settings(worktrees),
            worktrees.copy_files.clone(),
            worktrees.setup_command.clone(),
            shared_target,
        ),
        agent: with_compiler_cache(agent, worktrees.share_build_cache),
        bypass_modes,
    })
}

/// Runs native selection, asking the implementation session for a preference only
/// when a custom goal needs interpretation.
#[allow(clippy::too_many_arguments)]
pub fn choose_and_claim_selecting(
    store: &Mutex<StateStore>,
    channel: &Channel,
    project_id: u32,
    thread_id: u32,
    slot: &GoalAgentSlot,
    worktree_path: &Path,
    initial_branch: &str,
    timestamp: &str,
    subsystems: Option<&SubsystemMap>,
    selector: &mut dyn GoalChoice,
) -> Result<ClaimPhaseOutcome, GoalSwarmError> {
    let project = read_project(store, project_id)?;
    let (member, nick) = thread_identity(&project, thread_id, &slot.nick);
    let context = ClaimContext {
        store,
        channel,
        member,
        nick,
        project_id,
        thread_id,
        slot,
        worktree_path,
        initial_branch,
        timestamp,
        rosen: channel.server_backed() || project.rosen_in_use(),
        #[cfg(test)]
        before_claim: None,
    };
    let inventory = read_goal_work_pool(&project, slot.sources, worktree_path)?;
    let candidates = candidates(
        inventory.pool,
        worktree_path,
        inventory.issue_files,
        inventory.issue_versions,
        subsystems,
    );
    context.choose(&project, &candidates, subsystems, Some(selector))
}

/// Runs the native claim protocol with an injected pool source.
#[allow(clippy::too_many_arguments)]
pub fn choose_and_claim_with<Reader>(
    store: &Mutex<StateStore>,
    channel: &Channel,
    project_id: u32,
    thread_id: u32,
    slot: &GoalAgentSlot,
    worktree_path: &Path,
    initial_branch: &str,
    timestamp: &str,
    pool_reader: Reader,
) -> Result<ClaimPhaseOutcome, GoalSwarmError>
where
    Reader: Fn(&ProjectItem) -> Result<WorkPool, WorkPoolError>,
{
    let project = read_project(store, project_id)?;
    let (member, nick) = thread_identity(&project, thread_id, &slot.nick);
    let context = ClaimContext {
        store,
        channel,
        member,
        nick,
        project_id,
        thread_id,
        slot,
        worktree_path,
        initial_branch,
        timestamp,
        rosen: channel.server_backed() || project.rosen_in_use(),
        #[cfg(test)]
        before_claim: None,
    };
    let pool = slot.sources.filter(pool_reader(&project)?);
    let candidates = candidates(
        pool,
        worktree_path,
        Default::default(),
        Default::default(),
        None,
    );
    context.choose(&project, &candidates, None, None)
}

fn thread_identity(project: &ProjectItem, thread_id: u32, fallback_nick: &str) -> (String, String) {
    if let Some(thread) = project.threads.iter().find(|item| item.id == thread_id) {
        (thread.airc_member(), thread.agent_name())
    } else {
        let name = velocity_core::thread_titles::thread_agent_name(fallback_nick, thread_id);
        (name.clone(), name)
    }
}

/// Evaluates `keep_going` after `merged_thread` lands (`SPEC.md` F16 step 5).
pub fn evaluate_keep_going<Reader>(
    store: &Mutex<StateStore>,
    channel: &Channel,
    project_id: u32,
    merged_thread: u32,
    timestamp: &str,
    pool_reader: Reader,
) -> Result<KeepGoingOutcome, GoalSwarmError>
where
    Reader: Fn(&ProjectItem) -> Result<WorkPool, WorkPoolError>,
{
    let slot = {
        let mut guard = store
            .lock()
            .map_err(|error| GoalSwarmError::LockPoisoned(error.to_string()))?;
        guard.prepare_keep_going(project_id, merged_thread)?
    };
    let Some(slot) = slot else {
        return Ok(KeepGoingOutcome::Stopped);
    };
    let project = read_project(store, project_id)?;
    let pool = slot.sources.filter(pool_reader(&project)?);
    let flags = open_flags_for_pool(&pool, &held_keys(channel)?);
    match decide_after_merge(slot.keep_going, &flags) {
        PostMergeDecision::Stop => Ok(KeepGoingOutcome::Stopped),
        PostMergeDecision::Continue => Ok(KeepGoingOutcome::NextRound(slot)),
        PostMergeDecision::LeaveEmpty => {
            finish_keep_going_leave(store, channel, project_id, &slot, timestamp)
        }
    }
}

/// Builds the full agent prompt: the user's exact prompt plus Velocity's locked F16
/// lifecycle and Rule 8 implementation-first proof instructions (`Fixes #73`).
pub fn goal_agent_prompt(
    slot: &GoalAgentSlot,
    project: &ProjectItem,
    piece: &WorkPiece,
    worktree_path: &Path,
    _branch_name: &str,
) -> String {
    let prompt = rosen_goal_prompt(slot, project, piece, worktree_path);
    velocity_core::text_budget::prompt_excerpt(
        &prompt,
        velocity_core::text_budget::PROMPT_BYTES,
        "\n[Context excerpt. Read the claimed task, issue or spec section for its full details.]",
    )
}

fn rosen_goal_prompt(
    slot: &GoalAgentSlot,
    project: &ProjectItem,
    piece: &WorkPiece,
    workspace: &Path,
) -> String {
    let read_issue = match &piece.key {
        WorkKey::Issue { repository, number } => {
            format!("Read the issue first with `gh issue view {number} --repo {repository}`. ")
        }
        _ => String::new(),
    };
    let spec = piece_spec(piece);
    format!(
        "{}\n\n---\nVelocity goal swarm context (SPEC.md F16):\n\
         - Nick: `{}` in `#{}`; workspace: `{}`.\n\
         - Claimed piece: {} ({}). {read_issue}Name this piece in your Rosen snapshot summary.\n\
         - This is a Rosen workspace. Claim code paths with `rosen claim --exclusive <paths>` (or `claim` with `exclusive: true` on the `rosen` MCP server; plain `rosen claim <paths>` notices are for registration lists and generated files), record overlaps with `rosen share`, and name completed work with `rosen commit -m \"<summary>\"`, then end your turn. The task is done when the change lands.\n\
         - For builds and tests that can take more than a minute, use `queue_run` from `velocity-run` and end the turn until the result arrives. Use `rosen run -- <command>` for other builds and tests or when the tool is unavailable.\n\
         - Use the task filter in `airc_claims` to check work-item ownership. If this task depends on another member's held paths, call the `wait` tool on the `rosen` MCP server and end your turn. Velocity continues your work on a stacked change (Rosen D19) or resumes you once the paths are free.\n\
         - Implement and run first. Follow the project's proof-list instructions.\n\n{spec}",
        goal_excerpt(&slot.prompt),
        slot.nick,
        project.name,
        workspace.display(),
        piece.title,
        piece.reference
    )
}

fn goal_excerpt(goal: &str) -> String {
    velocity_core::text_budget::prompt_excerpt(goal, 2_500, "\n[Goal excerpt]")
}

fn piece_spec(piece: &WorkPiece) -> String {
    match &piece.key {
        WorkKey::Spec { section } => format!(
            "Spec work item: `{section}`. {}",
            crate::task_spec::instructions(&[&piece.title, section])
        ),
        _ => crate::task_spec::instructions(&[&piece.title]),
    }
}

impl ClaimContext<'_> {
    fn choose(
        self,
        project: &ProjectItem,
        candidates: &[Candidate],
        subsystems: Option<&SubsystemMap>,
        mut selector: Option<&mut dyn GoalChoice>,
    ) -> Result<ClaimPhaseOutcome, GoalSwarmError> {
        let change = project
            .threads
            .iter()
            .find(|thread| thread.id == self.thread_id)
            .and_then(|thread| thread.rosen_change.as_deref());
        register_and_join(
            self.channel,
            &self.member,
            &self.nick,
            project,
            self.worktree_path,
            self.initial_branch,
            change,
        )?;
        let held = HeldWork::new(
            self.channel.goal_claims(&self.member)?,
            candidates,
            subsystems,
        );
        let mut available: Vec<_> = candidates
            .iter()
            .filter(|piece| held.allows(piece))
            .collect();
        if let Some(selector) = selector.as_deref_mut()
            && needs_choice(&self.slot.prompt)
            && !available.is_empty()
        {
            let ranked = shortlist(&self.slot.prompt, available.into_iter());
            let pieces: Vec<_> = ranked.iter().map(|candidate| &candidate.piece).collect();
            available = selected_candidates(&ranked, selector.choose(&pieces)?)?;
        }
        let mut feed_events = Vec::new();
        for candidate in available {
            match self.try_claim(candidate, candidates, subsystems)? {
                Some(ClaimResult::Granted { .. }) => {
                    if let Some(claimed) = self.finalize_grant(candidate, &mut feed_events)? {
                        return Ok(ClaimPhaseOutcome::Claimed(Box::new(claimed)));
                    }
                }
                Some(ClaimResult::Refused { holder, .. }) => {
                    feed_events.push(self.record_refusal(&candidate.piece, &holder)?);
                }
                None => {}
            }
        }
        if let Some(selector) = selector {
            selector.stop()?;
        }
        self.leave_empty(project, feed_events)
    }

    fn try_claim(
        &self,
        candidate: &Candidate,
        candidates: &[Candidate],
        subsystems: Option<&SubsystemMap>,
    ) -> Result<Option<ClaimResult>, GoalSwarmError> {
        let _step = (self.rosen || subsystems.is_some())
            .then(|| SUBSYSTEM_CLAIM_STEP.lock())
            .transpose()
            .map_err(|error| GoalSwarmError::LockPoisoned(error.to_string()))?;
        let held = HeldWork::new(
            self.channel.goal_claims(&self.member)?,
            candidates,
            subsystems,
        );
        if !held.allows(candidate) {
            return Ok(None);
        }
        let piece = &candidate.piece;
        #[cfg(test)]
        if let Some(before_claim) = self.before_claim {
            before_claim(piece)?;
        }
        // On Rosen projects, the initial AIRC reservation carries work-item identity only
        // (no branch or path claims); Rosen owns path claims directly (F14, #531, #673).
        let result = if self.rosen {
            self.channel
                .claim_work_item(&self.member, &piece.key.claim_piece(), &piece.title)?
        } else {
            self.channel.claim_goal_piece(
                &self.member,
                &piece.key.claim_piece(),
                &piece.title,
                self.initial_branch,
                candidate.files.clone(),
            )?
        };
        Ok(Some(result))
    }

    fn finalize_grant(
        &self,
        candidate: &Candidate,
        feed_events: &mut Vec<GoalFeedEvent>,
    ) -> Result<Option<ClaimedGoalWork>, GoalSwarmError> {
        let piece = &candidate.piece;
        let key = piece.key.claim_piece();
        let issue_version = candidate
            .issue_version
            .as_ref()
            .map(OwnedIssueVersion::as_version)
            .unwrap_or_default();
        let Some((project, final_branch)) = self.reserve_store_claim(piece, issue_version, &key)?
        else {
            return Ok(None);
        };
        let change = project
            .threads
            .iter()
            .find(|thread| thread.id == self.thread_id)
            .and_then(|thread| thread.rosen_change.as_deref());
        register_and_join(
            self.channel,
            &self.member,
            &self.nick,
            &project,
            self.worktree_path,
            &final_branch,
            change,
        )?;
        let suffix = match &piece.key {
            WorkKey::Issue { number, .. } => format!(" \u{00b7} #{number}"),
            _ => String::new(),
        };
        feed_events.push(record_feed(
            self.store,
            self.project_id,
            self.slot.swarm_id,
            GoalFeedEvent::new(
                self.timestamp,
                &self.slot.nick,
                GoalEventKind::Claim,
                format!("{} claimed {}{suffix}", self.slot.nick, piece.title),
            ),
        )?);
        let prompt = goal_agent_prompt(
            self.slot,
            &project,
            piece,
            self.worktree_path,
            &final_branch,
        );
        Ok(Some(ClaimedGoalWork {
            piece: piece.clone(),
            branch_name: final_branch,
            prompt,
            feed_events: std::mem::take(feed_events),
        }))
    }

    fn reserve_store_claim(
        &self,
        piece: &WorkPiece,
        issue_version: IssueVersion<'_>,
        key: &str,
    ) -> Result<Option<(ProjectItem, String)>, GoalSwarmError> {
        let mut guard = self
            .store
            .lock()
            .map_err(|error| GoalSwarmError::LockPoisoned(error.to_string()))?;
        let project = guard
            .project(self.project_id)
            .cloned()
            .ok_or(ThreadError::UnknownProject(self.project_id))?;
        let target_branch = self.initial_branch.to_owned();
        // Recheck persisted work history (and legacy completed_goal_issues) against the
        // selection's source version under the store lock so reopened issues can be claimed
        // while unchanged mirror-lagged issues and competing claims are still refused (#638).
        match guard.claim_goal_piece_for_version(
            self.project_id,
            self.thread_id,
            piece,
            target_branch.clone(),
            issue_version,
        ) {
            Ok(()) => Ok(Some((project, target_branch))),
            Err(error) => {
                self.channel.release(&self.member, key)?;
                match error {
                    ThreadError::UnknownTask(_)
                    | ThreadError::TaskUnavailable(_)
                    | ThreadError::IssueAssigned { .. }
                    | ThreadError::IssueCompleted(_)
                    | ThreadError::SpecUnavailable(_) => Ok(None),
                    other => Err(other.into()),
                }
            }
        }
    }

    fn record_refusal(
        &self,
        piece: &WorkPiece,
        holder: &str,
    ) -> Result<GoalFeedEvent, GoalSwarmError> {
        record_feed(
            self.store,
            self.project_id,
            self.slot.swarm_id,
            GoalFeedEvent::new(
                self.timestamp,
                &self.slot.nick,
                GoalEventKind::Refused,
                format!(
                    "{}'s claim on {} was refused \u{00b7} {holder} has it",
                    self.slot.nick, piece.title
                ),
            ),
        )
    }

    fn leave_empty(
        &self,
        project: &ProjectItem,
        mut feed_events: Vec<GoalFeedEvent>,
    ) -> Result<ClaimPhaseOutcome, GoalSwarmError> {
        let _ = self.channel.leave(&self.member);
        {
            let mut guard = self
                .store
                .lock()
                .map_err(|error| GoalSwarmError::LockPoisoned(error.to_string()))?;
            guard.finish_goal_member_leave(
                project.id,
                self.slot.swarm_id,
                &self.slot.nick,
                Some(self.thread_id),
                "nothing unclaimed matches the goal and work sources".into(),
            )?;
        }
        let mut left_event =
            Event::agent_left(&self.nick, "nothing unclaimed \u{00b7} worktree removed")?;
        if let Some(change) = project
            .threads
            .iter()
            .find(|thread| thread.id == self.thread_id)
            .and_then(|thread| thread.rosen_change.as_deref())
        {
            left_event = left_event.for_change(change);
        }
        let left_entry = self.channel.post_event_once(left_event)?;
        feed_events.push(record_feed(
            self.store,
            project.id,
            self.slot.swarm_id,
            GoalFeedEvent::new(
                self.timestamp,
                &self.slot.nick,
                GoalEventKind::Left,
                format!(
                    "{} found nothing unclaimed and left \u{00b7} worktree removed",
                    self.slot.nick
                ),
            ),
        )?);
        Ok(ClaimPhaseOutcome::Left {
            feed_events,
            left_entry: left_entry.map(Box::new),
        })
    }
}

fn selected_candidates<'a>(
    shortlist: &[&'a Candidate],
    keys: Vec<String>,
) -> Result<Vec<&'a Candidate>, GoalSwarmError> {
    let mut seen = BTreeSet::new();
    keys.into_iter()
        .filter(|key| seen.insert(key.clone()))
        .map(|key| {
            shortlist
                .iter()
                .copied()
                .find(|candidate| candidate.piece.key.claim_piece() == key)
                .ok_or_else(|| {
                    GoalSwarmError::Selection("The choice is outside the shortlist".into())
                })
        })
        .collect()
}

fn finish_keep_going_leave(
    store: &Mutex<StateStore>,
    channel: &Channel,
    project_id: u32,
    slot: &GoalAgentSlot,
    timestamp: &str,
) -> Result<KeepGoingOutcome, GoalSwarmError> {
    let reason = "done \u{00b7} nothing unclaimed matches the goal and work sources";
    {
        let mut guard = store
            .lock()
            .map_err(|error| GoalSwarmError::LockPoisoned(error.to_string()))?;
        guard.finish_goal_member_leave(
            project_id,
            slot.swarm_id,
            &slot.nick,
            None,
            reason.into(),
        )?;
    }
    let left_entry = channel.post_event_once(Event::agent_left(&slot.nick, reason)?)?;
    let feed_event = record_feed(
        store,
        project_id,
        slot.swarm_id,
        GoalFeedEvent::new(
            timestamp,
            &slot.nick,
            GoalEventKind::Left,
            format!(
                "{} finished \u{00b7} nothing unclaimed matches the goal and work sources",
                slot.nick
            ),
        ),
    )?;
    Ok(KeepGoingOutcome::LeftEmpty {
        swarm_id: slot.swarm_id,
        feed_event,
        left_entry: left_entry.map(Box::new),
    })
}

fn read_project(store: &Mutex<StateStore>, project_id: u32) -> Result<ProjectItem, GoalSwarmError> {
    store
        .lock()
        .map_err(|error| GoalSwarmError::LockPoisoned(error.to_string()))?
        .project(project_id)
        .cloned()
        .ok_or(GoalSwarmError::Store(ThreadError::UnknownProject(
            project_id,
        )))
}

fn held_keys(channel: &Channel) -> Result<BTreeSet<String>, velocity_airc::Error> {
    Ok(channel
        .claims()?
        .into_iter()
        .map(|claim| claim.key().to_owned())
        .collect())
}

fn register_and_join(
    channel: &Channel,
    member: &str,
    nick: &str,
    project: &ProjectItem,
    worktree_path: &Path,
    branch: &str,
    change: Option<&str>,
) -> Result<(), velocity_airc::Error> {
    let display = worktree_path.display().to_string();
    let checkout = match change {
        Some(change) => Checkout::rosen(
            project.repository_path.clone(),
            display,
            Some(change.to_owned()),
            base_branch(project),
        ),
        None => Checkout::new(
            project.repository_path.clone(),
            display,
            branch.into(),
            base_branch(project),
        ),
    };
    channel.register_checkout(member, checkout)?;
    channel.join(member, nick)?;
    Ok(())
}

fn record_feed(
    store: &Mutex<StateStore>,
    project_id: u32,
    swarm_id: u32,
    event: GoalFeedEvent,
) -> Result<GoalFeedEvent, GoalSwarmError> {
    store
        .lock()
        .map_err(|error| GoalSwarmError::LockPoisoned(error.to_string()))?
        .record_goal_swarm_event(project_id, swarm_id, event.clone())?;
    Ok(event)
}

fn nick_taken(project: &ProjectItem, location: &Location, slug: &str) -> bool {
    let branch = format!("vel/{slug}");
    let folder_taken = match location.job_worktree(&velocity_git::JobKind::Branch(slug.to_owned()))
    {
        Ok(path) => path.is_some_and(|path| {
            path.exists()
                || path
                    .parent()
                    .is_some_and(|parent| parent.join(".rounds").join(slug).exists())
        }),
        Err(WorktreeError::ReservedLeafName(_)) => true,
        Err(_) => false,
    };
    folder_taken
        || project
            .threads
            .iter()
            .any(|thread| thread.branch_name == branch)
}

fn member_workspace_path(
    location: &Location,
    leaf: &str,
    nick: &str,
) -> Result<PathBuf, WorktreeError> {
    let path = location
        .job_worktree(&velocity_git::JobKind::Branch(leaf.into()))?
        .ok_or(WorktreeError::EmptyLeafName)?;
    // A landed round can reopen after a build break. Never reuse its directory for
    // the next round, and keep the nick as the leaf so AIRC's identity stays stable.
    if leaf != nick {
        Ok(path.with_file_name(".rounds").join(leaf).join(nick))
    } else {
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use velocity_airc::Channel;
    use velocity_core::goal_swarm::GoalEventKind;
    use velocity_core::work_pool::{WorkKey, WorkPiece, WorkPool, WorkSources, WorkStatus};
    use velocity_store::goal_swarms::{GoalAgentSlot, GoalSwarmMember, GoalSwarmRecord};
    use velocity_store::issues::ThreadIssue;
    use velocity_store::rosen::RosenProgress;
    use velocity_store::rosen_work::{OwnedIssueVersion, WorkEvidence, parse_timestamp_seconds};
    use velocity_store::threads::NewThread;
    use velocity_store::{
        EffortLevel, PersistedState, ProjectItem, ProviderKind, SpeedMode, StateStore, ThreadItem,
        ThreadState, VersionControl,
    };

    use super::{ClaimContext, ClaimPhaseOutcome, read_project, thread_identity};
    use crate::goal_selection::candidates;

    struct ReopenedSwarmFixture {
        root: PathBuf,
        store: Mutex<StateStore>,
        channel: Channel,
        slot: GoalAgentSlot,
    }

    // Risk (#638): Goal Swarm selection admits a reopened issue using its source version,
    // so reservation must carry that version through bind_piece_owner while still refusing
    // unchanged mirror-lagged issues and competing claims without leaking AIRC locks.
    #[test]
    fn goal_swarm_claims_reopened_issue_and_refuses_mirror_lag_and_competing_claims()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = seed_reopened_swarm()?;
        let worktree = fixture.root.join("wt");
        let stale_version = OwnedIssueVersion {
            updated_at: Some("2026-11-18T11:30:00Z".into()),
            reopened_at: None,
            title: Some("Fix issue".into()),
            body: Some("Original body".into()),
        };
        let reopened_version = OwnedIssueVersion {
            updated_at: Some("2026-11-21T10:00:00Z".into()),
            reopened_at: Some("2026-11-21T10:00:00Z".into()),
            title: Some("Fix issue".into()),
            body: Some("Reopened body".into()),
        };

        let stale_outcome = run_claim(&fixture, &worktree, 1, stale_version)?;
        assert!(matches!(stale_outcome, ClaimPhaseOutcome::Left { .. }));
        assert!(fixture.channel.claims()?.is_empty());

        let reopened_outcome = run_claim(&fixture, &worktree, 2, reopened_version.clone())?;
        assert!(matches!(reopened_outcome, ClaimPhaseOutcome::Claimed(_)));
        assert_eq!(fixture.channel.claims()?.len(), 1);
        let project = read_project(&fixture.store, 1)?;
        let key = WorkKey::issue("owner/repo", 7);
        let history: Vec<WorkStatus> = project
            .rosen_work_items
            .iter()
            .filter(|item| item.key == key)
            .map(|item| item.status)
            .collect();
        assert_eq!(history, vec![WorkStatus::Green, WorkStatus::Claimed]);

        let competing_outcome = run_claim(&fixture, &worktree, 3, reopened_version)?;
        assert!(matches!(competing_outcome, ClaimPhaseOutcome::Left { .. }));
        assert_eq!(fixture.channel.claims()?.len(), 1);

        std::fs::remove_dir_all(fixture.root)?;
        Ok(())
    }

    fn run_claim(
        fixture: &ReopenedSwarmFixture,
        worktree: &Path,
        round: u32,
        version: OwnedIssueVersion,
    ) -> Result<ClaimPhaseOutcome, Box<dyn std::error::Error>> {
        let mut round_slot = fixture.slot.clone();
        round_slot.nick = format!("codex-{round}");
        let thread_id = {
            let mut guard = fixture.store.lock().map_err(|error| error.to_string())?;
            let thread = NewThread::new(
                "Choosing work in AIRC".into(),
                format!("vel/{}", round_slot.nick),
                worktree.display().to_string(),
                &guard.settings(),
            );
            guard.start_goal_member_thread(1, &round_slot, thread)?
        };
        let project = read_project(&fixture.store, 1)?;
        let (member, nick) = thread_identity(&project, thread_id, &round_slot.nick);
        let pool = WorkPool::new(
            1,
            vec![WorkPiece::new(
                WorkKey::issue("Owner/Repo", 7),
                "Fix issue".into(),
                "https://example.invalid/7".into(),
                None,
            )],
        );
        let mut versions = HashMap::new();
        versions.insert(7, version);
        let candidate_list = candidates(pool, worktree, HashMap::new(), versions, None);
        let context = ClaimContext {
            store: &fixture.store,
            channel: &fixture.channel,
            member,
            nick,
            project_id: 1,
            thread_id,
            slot: &round_slot,
            worktree_path: worktree,
            initial_branch: "vel/work",
            timestamp: "10:00",
            rosen: fixture.channel.server_backed() || project.rosen_in_use(),
            before_claim: None,
        };
        Ok(context.choose(&project, &candidate_list, None, None)?)
    }

    fn seed_reopened_swarm() -> Result<ReopenedSwarmFixture, Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("velocity-goal-638-{unique}"));
        let state_dir = root.join("state");
        std::fs::create_dir_all(root.join("repo"))?;
        std::fs::create_dir_all(root.join("wt"))?;
        std::fs::create_dir_all(&state_dir)?;
        let mut persisted = PersistedState::default();
        persisted.projects.push(seed_reopened_project(&root));
        std::fs::write(
            state_dir.join("state.json"),
            serde_json::to_vec(&persisted)?,
        )?;
        let mut store = StateStore::load_or_default(&state_dir)?;
        record_prior_green_issue(&mut store)?;
        let slot = GoalAgentSlot {
            swarm_id: 10,
            prompt: "Implement the next unclaimed piece.".into(),
            sources: WorkSources::default(),
            nick: "codex-1".into(),
            provider: ProviderKind::Codex,
            model_name: "gpt-5.4".into(),
            effort: EffortLevel::High,
            speed: SpeedMode::Regular,
            keep_going: false,
            previous_work_thread: None,
        };
        let channel = Channel::open(&root.join("airc"), "1")?;
        Ok(ReopenedSwarmFixture {
            root,
            store: Mutex::new(store),
            channel,
            slot,
        })
    }

    fn seed_reopened_project(root: &Path) -> ProjectItem {
        let issue = ThreadIssue::new("Owner/Repo".into(), 7);
        let mut project =
            ProjectItem::new(1, "Demo".into(), root.join("repo").display().to_string());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        project.completed_goal_issues.push(issue.clone());
        let mut original = ThreadItem::new(4, "Fix issue".into());
        original.state = ThreadState::Working;
        original.issue = Some(issue);
        project.threads.push(original);
        project.goal_swarms.push(GoalSwarmRecord {
            id: 10,
            prompt: "Implement the next unclaimed piece.".into(),
            sources: WorkSources::default(),
            keep_going: false,
            started: "09:00".into(),
            mix_summary: "1 Codex".into(),
            members: ["codex-1", "codex-2", "codex-3"]
                .into_iter()
                .map(|nick| GoalSwarmMember {
                    nick: nick.into(),
                    provider: ProviderKind::Codex,
                    model_name: "gpt-5.4".into(),
                    effort: EffortLevel::High,
                    speed: SpeedMode::Regular,
                    thread: None,
                    left_reason: None,
                    merged_threads: Vec::new(),
                })
                .collect(),
            feed: Vec::new(),
        });
        project
    }

    fn record_prior_green_issue(store: &mut StateStore) -> Result<(), Box<dyn std::error::Error>> {
        store.update_rosen_work_thread(1, 4, |thread| {
            thread.rosen_change = Some("original".into());
        })?;
        store.save_rosen_work_progress(
            1,
            1,
            "original",
            (RosenProgress::Landed, ""),
            WorkEvidence {
                commit: Some("4a63d3f"),
                revert_reason: None,
                occurred_at_ms: None,
            },
        )?;
        store.confirm_rosen_work_build(1, 2, &["4a63d3f".into()])?;
        store.remove_thread(1, 4)?;
        let nov_18 = parse_timestamp_seconds("2026-11-18T12:00:00Z")
            .ok_or("valid timestamp")?
            .to_string();
        store.update_project(1, |project| {
            if let Some(work) = project
                .rosen_work_items
                .iter_mut()
                .find(|item| item.key == WorkKey::issue("owner/repo", 7))
            {
                work.landed_at = Some(nov_18.clone());
                work.green_at = Some(nov_18);
            }
            Ok::<_, velocity_store::projects::ProjectError>(())
        })?;
        Ok(())
    }

    struct ReservationCase {
        rosen: bool,
        slots: Vec<(u32, Vec<String>)>,
        held_pieces: Vec<u32>,
        held_paths: Vec<String>,
        race_winners: Vec<u32>,
        expected: ExpectedReservation,
    }

    enum ExpectedReservation {
        Claimed {
            index: usize,
            piece_id: u32,
            sends_branch: bool,
            paths: Vec<String>,
            refusals: Vec<usize>,
        },
        Left {
            refusals: Vec<usize>,
        },
    }

    fn parse_nat_list(raw: &str) -> Result<Vec<u32>, Box<dyn std::error::Error>> {
        if raw == "-" {
            return Ok(Vec::new());
        }
        raw.split(',')
            .map(|value| Ok(value.parse::<u32>()?))
            .collect()
    }

    fn file_paths(ids: Vec<u32>) -> Vec<String> {
        let mut paths: Vec<String> = ids
            .into_iter()
            .map(|id| format!("src/file_{id}.rs"))
            .collect();
        paths.sort();
        paths
    }

    fn parse_reservation_row(
        fields: &[&str],
    ) -> Result<ReservationCase, Box<dyn std::error::Error>> {
        let rosen = match fields[1] {
            "rosen" => true,
            "git" => false,
            other => return Err(format!("unknown channel mode {other}").into()),
        };
        let slots = fields[2]
            .split(';')
            .map(
                |entry| -> Result<(u32, Vec<String>), Box<dyn std::error::Error>> {
                    let (id, files) = entry.split_once(':').ok_or("missing candidate separator")?;
                    Ok((id.parse()?, file_paths(parse_nat_list(files)?)))
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        let board: Vec<_> = fields[3].split('/').collect();
        if board.len() != 3 {
            return Err(format!("malformed reservation board {}", fields[3]).into());
        }
        let outcome: Vec<_> = fields[4].split(':').collect();
        let expected = match outcome.as_slice() {
            ["claimed", idx, piece_id, sends_branch, paths, refusals] => {
                ExpectedReservation::Claimed {
                    index: idx.parse()?,
                    piece_id: piece_id.parse()?,
                    sends_branch: sends_branch.parse()?,
                    paths: file_paths(parse_nat_list(paths)?),
                    refusals: parse_nat_list(refusals)?
                        .into_iter()
                        .map(|n| n as usize)
                        .collect(),
                }
            }
            ["left", refusals] => ExpectedReservation::Left {
                refusals: parse_nat_list(refusals)?
                    .into_iter()
                    .map(|n| n as usize)
                    .collect(),
            },
            _ => return Err(format!("malformed reservation outcome {}", fields[4]).into()),
        };
        Ok(ReservationCase {
            rosen,
            slots,
            held_pieces: parse_nat_list(board[0])?,
            held_paths: file_paths(parse_nat_list(board[1])?),
            race_winners: parse_nat_list(board[2])?,
            expected,
        })
    }

    fn seed_reservation_project(root: &Path, rosen: bool) -> ProjectItem {
        let mut project =
            ProjectItem::new(1, "Demo".into(), root.join("repo").display().to_string());
        project.version_control = if rosen {
            VersionControl::Rosen
        } else {
            VersionControl::Git
        };
        project.spec_locked = true;
        project.goal_swarms.push(GoalSwarmRecord {
            id: 10,
            prompt: "Implement the next unclaimed piece.".into(),
            sources: WorkSources::default(),
            keep_going: false,
            started: "09:00".into(),
            mix_summary: "1 Codex".into(),
            members: vec![GoalSwarmMember {
                nick: "codex-1".into(),
                provider: ProviderKind::Codex,
                model_name: "gpt-5.4".into(),
                effort: EffortLevel::High,
                speed: SpeedMode::Regular,
                thread: None,
                left_reason: None,
                merged_threads: Vec::new(),
            }],
            feed: Vec::new(),
        });
        project
    }

    fn seed_reservation_case(
        row_index: usize,
        case: &ReservationCase,
    ) -> Result<(ReopenedSwarmFixture, u32), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("velocity-goal-673-{row_index}-{unique}"));
        let state_dir = root.join("state");
        std::fs::create_dir_all(root.join("repo"))?;
        std::fs::create_dir_all(root.join("wt"))?;
        std::fs::create_dir_all(&state_dir)?;
        let mut persisted = PersistedState::default();
        persisted
            .projects
            .push(seed_reservation_project(&root, case.rosen));
        std::fs::write(
            state_dir.join("state.json"),
            serde_json::to_vec(&persisted)?,
        )?;
        let mut store = StateStore::load_or_default(&state_dir)?;
        let slot = GoalAgentSlot {
            swarm_id: 10,
            prompt: "Implement the next unclaimed piece.".into(),
            sources: WorkSources::default(),
            nick: "codex-1".into(),
            provider: ProviderKind::Codex,
            model_name: "gpt-5.4".into(),
            effort: EffortLevel::High,
            speed: SpeedMode::Regular,
            keep_going: false,
            previous_work_thread: None,
        };
        let thread = NewThread::new(
            "Choosing work in AIRC".into(),
            "vel/codex-1".into(),
            root.join("wt").display().to_string(),
            &store.settings(),
        );
        let thread_id = store.start_goal_member_thread(1, &slot, thread)?;
        if case.rosen {
            store.update_thread(1, thread_id, |saved| {
                saved.rosen_change = Some("chg-codex-1".into());
            })?;
        }
        let channel = Channel::open(&root.join("airc"), "1")?;
        seed_reservation_channel(&channel, case)?;
        Ok((
            ReopenedSwarmFixture {
                root,
                store: Mutex::new(store),
                channel,
                slot,
            },
            thread_id,
        ))
    }

    fn seed_reservation_channel(
        channel: &Channel,
        case: &ReservationCase,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !case.held_pieces.is_empty() {
            channel.join("pre-holder", "pre-holder")?;
            for id in &case.held_pieces {
                let _ = channel.claim_work_item(
                    "pre-holder",
                    &format!("issue:{id}"),
                    &format!("Piece {id}"),
                )?;
            }
        }
        if !case.held_paths.is_empty() {
            channel.join("path-holder", "path-holder")?;
            let _ = channel.claim_piece(
                "path-holder",
                "issue:999",
                "Path holder",
                "vel/path-holder",
                case.held_paths.clone(),
            )?;
        }
        if !case.race_winners.is_empty() {
            channel.join("racer", "racer")?;
        }
        Ok(())
    }

    fn refused_indices(
        events: &[velocity_core::goal_swarm::GoalFeedEvent],
        slots: &[(u32, Vec<String>)],
    ) -> Vec<usize> {
        events
            .iter()
            .filter(|event| event.kind == GoalEventKind::Refused)
            .filter_map(|event| {
                slots
                    .iter()
                    .position(|(id, _)| event.message.contains(&format!("Piece {id} was refused")))
            })
            .collect()
    }

    fn trigger_racer_claim(
        channel: &Channel,
        case: &ReservationCase,
        piece: &WorkPiece,
    ) -> Result<(), velocity_airc::Error> {
        let WorkKey::Issue { number, .. } = piece.key else {
            return Ok(());
        };
        if !case.race_winners.contains(&number) {
            return Ok(());
        }
        if case.rosen {
            let _ = channel.claim_work_item("racer", &piece.key.claim_piece(), &piece.title)?;
        } else {
            let files = case
                .slots
                .iter()
                .find(|(id, _)| *id == number)
                .map(|(_, files)| files.clone())
                .unwrap_or_default();
            let _ = channel.claim_goal_piece(
                "racer",
                &piece.key.claim_piece(),
                &piece.title,
                "vel/racer",
                files,
            )?;
        }
        Ok(())
    }

    fn verify_reservation_case(
        row_index: usize,
        case: &ReservationCase,
        line: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (fixture, thread_id) = seed_reservation_case(row_index, case)?;
        let worktree = fixture.root.join("wt");
        let project = read_project(&fixture.store, 1)?;
        let (member, nick) = thread_identity(&project, thread_id, &fixture.slot.nick);
        let pieces: Vec<_> = case
            .slots
            .iter()
            .map(|(id, _)| {
                WorkPiece::new(
                    WorkKey::issue("owner/repo", *id),
                    format!("Piece {id}"),
                    format!("https://example.invalid/{id}"),
                    None,
                )
            })
            .collect();
        let issue_files: HashMap<u32, Vec<String>> = case.slots.iter().cloned().collect();
        let candidate_list = candidates(
            WorkPool::new(1, pieces),
            &worktree,
            issue_files,
            HashMap::new(),
            None,
        );
        let race_hook = |piece: &WorkPiece| trigger_racer_claim(&fixture.channel, case, piece);
        let context = ClaimContext {
            store: &fixture.store,
            channel: &fixture.channel,
            member: member.clone(),
            nick,
            project_id: 1,
            thread_id,
            slot: &fixture.slot,
            worktree_path: &worktree,
            initial_branch: "vel/work",
            timestamp: "10:00",
            rosen: fixture.channel.server_backed() || project.rosen_in_use(),
            before_claim: Some(&race_hook),
        };
        let outcome = context.choose(&project, &candidate_list, None, None)?;
        assert_reservation_outcome(&fixture.channel, &member, case, outcome, line)?;
        std::fs::remove_dir_all(fixture.root)?;
        Ok(())
    }

    fn assert_reservation_outcome(
        channel: &Channel,
        member: &str,
        case: &ReservationCase,
        outcome: ClaimPhaseOutcome,
        line: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let member_claims: Vec<_> = channel
            .claims()?
            .into_iter()
            .filter(|claim| claim.member == member)
            .collect();
        match (&case.expected, outcome) {
            (
                ExpectedReservation::Claimed {
                    index,
                    piece_id,
                    sends_branch,
                    paths,
                    refusals,
                },
                ClaimPhaseOutcome::Claimed(claimed),
            ) => {
                assert_eq!(claimed.piece.key, WorkKey::issue("owner/repo", *piece_id));
                assert_eq!(case.slots[*index].0, *piece_id, "index mismatch for {line}");
                assert_eq!(
                    refused_indices(&claimed.feed_events, &case.slots),
                    *refusals
                );
                assert_eq!(member_claims.len(), 1, "expected one claim for {line}");
                let claim = &member_claims[0];
                assert_eq!(!claim.branch.is_empty(), *sends_branch, "branch: {line}");
                assert_eq!(claim.files, *paths, "claimed files mismatch for {line}");
                let stored = serde_json::to_value(claim)?;
                assert_eq!(stored.get("branch").is_some(), *sends_branch);
                assert_eq!(stored.get("files").is_some(), !paths.is_empty());
                Ok(())
            }
            (
                ExpectedReservation::Left { refusals },
                ClaimPhaseOutcome::Left { feed_events, .. },
            ) => {
                assert_eq!(refused_indices(&feed_events, &case.slots), *refusals);
                assert!(member_claims.is_empty(), "left member held claim: {line}");
                Ok(())
            }
            (_, other) => Err(format!("unexpected outcome {other:?} for {line}").into()),
        }
    }

    // Risk (#673): Rosen Goal Swarm's first AIRC reservation must send work-item identity alone
    // (no branch or path claims) while preserving known-file overlap checks and refusal retries
    // against racing candidates across the production reservation path.
    #[test]
    fn lean_reservation_fixtures() -> Result<(), Box<dyn std::error::Error>> {
        let mut reservation_rows = 0usize;
        for (row_index, line) in include_str!("../../../proofs/fixtures/goal-swarm-work-choice.tsv")
            .lines()
            .enumerate()
        {
            let fields: Vec<_> = line.split('\t').collect();
            if fields.first().copied() == Some("reservation") {
                reservation_rows += 1;
                let case = parse_reservation_row(&fields)?;
                verify_reservation_case(row_index, &case, line)?;
            }
        }
        assert!(reservation_rows >= 7);
        Ok(())
    }
}
