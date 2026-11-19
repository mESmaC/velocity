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
    PostMergeDecision, decide_after_merge, open_flags_for_pool, piece_branch_slug,
};
use velocity_core::work_pool::{WorkKey, WorkPiece, WorkPool};
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_git::{
    WorktreeError, WorktreeSetupRequest, local_branch_exists, remove_untouched_worktree,
    rename_worktree_branch,
};
use velocity_store::goal_swarms::{CHOOSING_THREAD_TITLE, GoalAgentSlot};
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
    #[error(transparent)]
    Cleanup(#[from] velocity_git::MergedWorktreeError),
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

struct ClaimContext<'a> {
    store: &'a Mutex<StateStore>,
    channel: &'a Channel,
    member: String,
    project_id: u32,
    thread_id: u32,
    slot: &'a GoalAgentSlot,
    worktree_path: &'a Path,
    initial_branch: &'a str,
    timestamp: &'a str,
}

/// Serialize the board read and claim between local members when using advisory subsystem
/// classification. The channel itself decides the atomic piece and file locks.
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
        nick_taken(project, &repository, &location, candidate)
    })
    .ok_or_else(|| LaunchError::NamesExhausted(slot.nick.clone()))?;
    let worktree_path = member_workspace_path(project, &location, &leaf, &slot.nick)?;
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
    let context = ClaimContext {
        store,
        channel,
        member: format!("thread-{thread_id}"),
        project_id,
        thread_id,
        slot,
        worktree_path,
        initial_branch,
        timestamp,
    };
    let project = read_project(store, project_id)?;
    let inventory = read_goal_work_pool(&project, slot.sources, worktree_path)?;
    let candidates = candidates(
        inventory.pool,
        worktree_path,
        inventory.issue_files,
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
    let context = ClaimContext {
        store,
        channel,
        member: format!("thread-{thread_id}"),
        project_id,
        thread_id,
        slot,
        worktree_path,
        initial_branch,
        timestamp,
    };
    let project = read_project(store, project_id)?;
    let pool = slot.sources.filter(pool_reader(&project)?);
    let candidates = candidates(pool, worktree_path, Default::default(), None);
    context.choose(&project, &candidates, None, None)
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
    branch_name: &str,
) -> String {
    let prompt = if worktree_path.join(".rosen").exists() {
        rosen_goal_prompt(slot, project, piece, worktree_path)
    } else {
        git_goal_prompt(slot, project, piece, worktree_path, branch_name)
    };
    velocity_core::text_budget::prompt_excerpt(
        &prompt,
        velocity_core::text_budget::PROMPT_BYTES,
        "\n[Context excerpt. Read the claimed task, issue or spec section for its full details.]",
    )
}

fn git_goal_prompt(
    slot: &GoalAgentSlot,
    project: &ProjectItem,
    piece: &WorkPiece,
    worktree_path: &Path,
    branch_name: &str,
) -> String {
    let default_branch = base_branch(project);
    let piece_instructions = match &piece.key {
        WorkKey::Task { identifier } => format!(
            "Task List item #{identifier}: {}\nName this Task List item in your PR description.",
            piece.title
        ),
        WorkKey::Issue { repository, number } => format!(
            "GitHub issue {repository}#{number}: {}\nRead it first with `gh issue view {number}`, including its evidence, and write `Fixes #{number}` in your PR description. Use issue_comment for starting work, findings that change the plan, and blockers.",
            piece.title
        ),
        WorkKey::Spec { section } => format!(
            "SPEC.md section `{section}`: {}\nName this SPEC.md item in your PR description. Velocity records it as built when your PR merges; never edit SPEC.md just to tick its checkbox.",
            piece.title
        ),
    };
    let spec = piece_spec(piece);
    format!(
        "{}\n\n---\nVelocity goal swarm context (locked, SPEC.md F16):\n\
         - Nick: `{}` in `#{}`\n\
         - Worktree: `{}` on branch `{branch_name}` (removed automatically after your PR merges)\n\
         - Claimed piece in AIRC: {piece_instructions}\n\
         - Work-pool context: {}. Prior pushed work is optional; never use another thread's checkout.\n\
         - Commit, push `{branch_name}`, open a PR against `{default_branch}`, and follow its checks and review: fix failed checks, answer every review comment, resolve addressed threads, and re-request review. Never merge your own PR.\n\
         - Use the path filter in `airc_claims` to check file ownership. Settle overlaps and dependency waits in AIRC; first claim wins.\n\
         - Implement and run the feature first; then write Lean proofs only if the feature is on the project's proof list in SPEC.md (Rule 8, F17).\n\n{spec}",
        goal_excerpt(&slot.prompt),
        slot.nick,
        project.name,
        worktree_path.display(),
        piece.reference
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
         - This is a Rosen workspace. Claim files with `rosen claim`, record overlaps with `rosen share`, and name completed work with `rosen commit -m \"<summary>\"`, then end your turn. The task is done when the change lands.\n\
         - For builds and tests that can take more than a minute, use `queue_run` from `velocity-run` and end the turn until the result arrives. Use `rosen run -- <command>` for other builds and tests or when the tool is unavailable.\n\
         - Use the task filter in `airc_claims` to check work ownership. If this task depends on another member's work, call `airc_wait` with its holder and file. Velocity continues your work on a stacked change (Rosen D19); the parent must land first.\n\
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
        register_and_join(
            self.channel,
            &self.member,
            project,
            self.worktree_path,
            self.initial_branch,
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
                    if let Some(claimed) =
                        self.finalize_grant(&candidate.piece, &mut feed_events)?
                    {
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
        let _step = subsystems
            .map(|_| SUBSYSTEM_CLAIM_STEP.lock())
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
        Ok(Some(self.channel.claim_goal_piece(
            &self.member,
            &piece.key.claim_piece(),
            &piece.title,
            self.initial_branch,
            candidate.files.clone(),
        )?))
    }

    fn finalize_grant(
        &self,
        piece: &WorkPiece,
        feed_events: &mut Vec<GoalFeedEvent>,
    ) -> Result<Option<ClaimedGoalWork>, GoalSwarmError> {
        let key = piece.key.claim_piece();
        let Some((project, planned_branch)) = self.reserve_store_claim(piece, &key)? else {
            return Ok(None);
        };
        let final_branch = self.settle_branch(&project, planned_branch)?;
        register_and_join(
            self.channel,
            &self.member,
            &project,
            self.worktree_path,
            &final_branch,
        )?;
        let _ = self.channel.claim_piece(
            &self.member,
            &key,
            &piece.title,
            &final_branch,
            Vec::new(),
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

    fn settle_branch(
        &self,
        project: &ProjectItem,
        planned: String,
    ) -> Result<String, GoalSwarmError> {
        if project
            .threads
            .iter()
            .any(|thread| thread.id == self.thread_id && thread.rosen_change.is_some())
        {
            return Ok(planned);
        }
        match rename_worktree_branch(
            Path::new(&project.repository_path),
            self.initial_branch,
            &planned,
        ) {
            Ok(()) => Ok(planned),
            Err(_) => {
                sync_fallback_branch(
                    self.store,
                    self.project_id,
                    self.thread_id,
                    self.initial_branch,
                )?;
                Ok(self.initial_branch.to_owned())
            }
        }
    }

    fn reserve_store_claim(
        &self,
        piece: &WorkPiece,
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
        let rosen = project
            .threads
            .iter()
            .any(|thread| thread.id == self.thread_id && thread.rosen_change.is_some());
        let target_branch = if rosen {
            self.initial_branch.to_owned()
        } else {
            unique_piece_branch(&project, piece, self.initial_branch)
        };
        if let WorkKey::Issue { repository, number } = &piece.key
            && project.completed_goal_issues.iter().any(|issue| {
                issue.number == *number && issue.repository.eq_ignore_ascii_case(repository)
            })
        {
            self.channel.release(&self.member, key)?;
            return Ok(None);
        }
        match guard.claim_goal_piece(
            self.project_id,
            self.thread_id,
            piece,
            target_branch.clone(),
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
        if !project
            .threads
            .iter()
            .any(|thread| thread.id == self.thread_id && thread.rosen_change.is_some())
        {
            remove_untouched_worktree(
                Path::new(&project.repository_path),
                self.worktree_path,
                self.initial_branch,
                &base_branch(project),
            )?;
        }
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
        let left_entry = self.channel.post_event_once(Event::agent_left(
            &self.slot.nick,
            "nothing unclaimed \u{00b7} worktree removed",
        )?)?;
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

fn unique_piece_branch(project: &ProjectItem, piece: &WorkPiece, current_branch: &str) -> String {
    let wanted = piece_branch_slug(piece);
    let repository = Path::new(&project.repository_path);
    let slug = unique_slug(&wanted, |candidate| {
        let branch = format!("vel/{candidate}");
        branch != current_branch
            && (project
                .threads
                .iter()
                .any(|thread| thread.branch_name == branch)
                || local_branch_exists(repository, &branch))
    });
    slug.map_or_else(|| current_branch.to_owned(), |slug| format!("vel/{slug}"))
}

fn sync_fallback_branch(
    store: &Mutex<StateStore>,
    project_id: u32,
    thread_id: u32,
    branch: &str,
) -> Result<(), GoalSwarmError> {
    store
        .lock()
        .map_err(|error| GoalSwarmError::LockPoisoned(error.to_string()))?
        .update_thread(project_id, thread_id, |thread| {
            thread.branch_name = branch.to_owned();
        })?;
    Ok(())
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
    project: &ProjectItem,
    worktree_path: &Path,
    branch: &str,
) -> Result<(), velocity_airc::Error> {
    let display = worktree_path.display().to_string();
    channel.register_checkout(
        member,
        Checkout::new(
            project.repository_path.clone(),
            display.clone(),
            branch.into(),
            base_branch(project),
        ),
    )?;
    channel.join(member, &display)?;
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

fn nick_taken(project: &ProjectItem, repository: &Path, location: &Location, slug: &str) -> bool {
    let branch = format!("vel/{slug}");
    let folder_taken = match location.job_worktree(&velocity_git::JobKind::Branch(slug.to_owned()))
    {
        Ok(path) => path.is_some_and(|path| {
            path.exists()
                || (project.version_control == velocity_store::VersionControl::Rosen
                    && path
                        .parent()
                        .is_some_and(|parent| parent.join(".rounds").join(slug).exists()))
        }),
        Err(WorktreeError::ReservedLeafName(_)) => true,
        Err(_) => false,
    };
    folder_taken
        || project.threads.iter().any(|thread| {
            (project.version_control == velocity_store::VersionControl::Rosen
                || thread.state != velocity_store::ThreadState::Merged)
                && thread.branch_name == branch
        })
        || (project.version_control == velocity_store::VersionControl::Git
            && local_branch_exists(repository, &branch))
}

fn member_workspace_path(
    project: &ProjectItem,
    location: &Location,
    leaf: &str,
    nick: &str,
) -> Result<PathBuf, WorktreeError> {
    let path = location
        .job_worktree(&velocity_git::JobKind::Branch(leaf.into()))?
        .ok_or(WorktreeError::EmptyLeafName)?;
    // A landed round can reopen after a build break. Never reuse its directory for
    // the next round, and keep the nick as the leaf so AIRC's identity stays stable.
    if project.version_control == velocity_store::VersionControl::Rosen && leaf != nick {
        Ok(path.with_file_name(".rounds").join(leaf).join(nick))
    } else {
        Ok(path)
    }
}
