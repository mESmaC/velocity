//! End-to-end headless verification of M9 Goal Swarm Launch (`SPEC.md` F16, `#73`).
//!
//! Run through the build queue:
//! `pwsh -NoProfile -File scripts/build-queue.ps1 cargo run -p velocity-threads --example goal_swarm`

// A console example: its children share its console, so no hidden_command (#232).
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Barrier, Mutex};

use velocity_airc::Channel;
use velocity_core::airc_modal::{AircMember, AircMemberState};
use velocity_core::work_pool::{WorkKey, WorkPiece, WorkPool};
use velocity_git::{execute_worktree_setup, remove_merged_worktree};
use velocity_github::PullRequestSnapshot;
use velocity_github::follow_through::FollowThrough;
use velocity_store::airc_members::airc_members;
use velocity_store::goal_swarm_dialog::GoalMixRow;
use velocity_store::goal_swarms::{GoalAgentSlot, LaunchGoalSwarmRequest};
use velocity_store::{
    EffortLevel, PersistedState, ProjectItem, ProviderKind, SpeedMode, StateStore, TaskAttribution,
    TaskItem, TaskState,
};
use velocity_threads::goal_swarm::{
    ClaimPhaseOutcome, ClaimedGoalWork, KeepGoingOutcome, choose_and_claim_with,
    evaluate_keep_going, plan_goal_member,
};

type Outcome<Value = ()> = Result<Value, Box<dyn std::error::Error>>;

struct Harness {
    root: PathBuf,
    repository: PathBuf,
    worktree_root: PathBuf,
    store: Mutex<StateStore>,
    channel: Channel,
}

struct PreparedSlot {
    slot: GoalAgentSlot,
    thread_id: u32,
    worktree_path: PathBuf,
    initial_branch: String,
}

fn main() -> Outcome {
    let harness = setup_harness()?;
    let result = run_scenario(&harness);
    let _ = std::fs::remove_dir_all(&harness.root);
    result
}

fn run_scenario(harness: &Harness) -> Outcome {
    let (swarm_id, slots) = open_swarm(harness)?;
    let prepared = create_choosing_worktrees(harness, &slots)?;
    verify_choosing_members(harness, &prepared)?;

    let claimed = run_concurrent_claims(harness, &prepared)?;
    verify_claims_and_leaves(harness, swarm_id, &prepared, &claimed)?;

    run_merges_and_keep_going(harness, swarm_id)?;
    println!("goal_swarm example: all F16 lifecycle assertions passed.");
    Ok(())
}

fn setup_harness() -> Outcome<Harness> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("velocity-goal-swarm-{stamp}"));
    let repository = root.join("repo");
    let worktree_root = root.join("wt");
    let state_dir = root.join("state");
    std::fs::create_dir_all(&repository)?;
    std::fs::create_dir_all(&worktree_root)?;
    std::fs::create_dir_all(&state_dir)?;
    init_git_repository(&repository)?;

    let mut persisted = PersistedState::default();
    persisted.settings.worktrees.location_root = Some(worktree_root.display().to_string());
    persisted.settings.worktrees.share_rust_build_cache = false;
    persisted.settings.worktrees.setup_command = String::new();
    persisted.settings.worktrees.copy_files = String::new();
    let mut project = ProjectItem::new(1, "veloc".into(), repository.display().to_string());
    project.default_branch = "master".into();
    project.spec_locked = true;
    project.tasks.push(TaskItem::new(
        1,
        "Implement goal launch".into(),
        TaskState::Todo,
        TaskAttribution::Spec,
    ));
    persisted.projects.push(project);
    std::fs::write(
        state_dir.join("state.json"),
        serde_json::to_string_pretty(&persisted)?,
    )?;

    let store = StateStore::load_or_default(&state_dir)?;
    let channel = Channel::open(&root.join("airc"), "1")?;
    Ok(Harness {
        root,
        repository,
        worktree_root,
        store: Mutex::new(store),
        channel,
    })
}

fn init_git_repository(repository: &Path) -> Outcome {
    run_git(repository, &["init", "-b", "master"])?;
    run_git(repository, &["config", "user.name", "Velocity Example"])?;
    run_git(
        repository,
        &["config", "user.email", "velocity@example.invalid"],
    )?;
    std::fs::write(
        repository.join("SPEC.md"),
        "# Spec\n\n## 7. Implementation plan\n**M9**\n- [ ] Goal swarm launch (F16)\n",
    )?;
    run_git(repository, &["add", "SPEC.md"])?;
    run_git(repository, &["commit", "-m", "Initial spec"])?;
    Ok(())
}

fn run_git(directory: &Path, args: &[&str]) -> Outcome<String> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn open_swarm(harness: &Harness) -> Outcome<(u32, Vec<GoalAgentSlot>)> {
    let request = LaunchGoalSwarmRequest::new(
        1,
        String::new(),
        vec![
            GoalMixRow::new(
                ProviderKind::Claude,
                EffortLevel::High,
                SpeedMode::Regular,
                2,
            ),
            GoalMixRow::new(ProviderKind::Codex, EffortLevel::High, SpeedMode::Fast, 2),
        ],
        true,
        false,
    );
    let mut store = harness
        .store
        .lock()
        .map_err(|error| format!("store lock: {error}"))?;
    let (swarm_id, slots) = store.open_goal_swarm(&request, "10:00".into())?;
    let nicks: Vec<_> = slots.iter().map(|slot| slot.nick.as_str()).collect();
    println!("launched swarm #{swarm_id} with nicks: {nicks:?}");
    if nicks != vec!["opus-1", "opus-2", "codex-1", "codex-2"] {
        return Err(format!("unexpected nick allocation: {nicks:?}").into());
    }
    Ok((swarm_id, slots))
}

fn create_choosing_worktrees(
    harness: &Harness,
    slots: &[GoalAgentSlot],
) -> Outcome<Vec<PreparedSlot>> {
    let mut prepared = Vec::new();
    for slot in slots {
        let (plan, thread_id) = {
            let mut store = harness
                .store
                .lock()
                .map_err(|error| format!("store lock: {error}"))?;
            let project = store.project(1).cloned().ok_or("missing project 1")?;
            let prefs = store.worktree_context().preferences;
            let settings = store.settings();
            let plan = plan_goal_member(&project, &prefs, &settings, slot)?;
            let thread_id = store.start_goal_member_thread(1, slot, plan.thread.clone())?;
            (plan, thread_id)
        };
        let created = execute_worktree_setup(&plan.worktree)?;
        let initial_branch = created
            .branch_name
            .ok_or("missing provisional branch on choosing worktree")?;
        println!(
            "slot {} -> thread #{thread_id} in {} on {initial_branch}",
            slot.nick,
            created.worktree_path.display()
        );
        prepared.push(PreparedSlot {
            slot: slot.clone(),
            thread_id,
            worktree_path: created.worktree_path,
            initial_branch,
        });
    }
    Ok(prepared)
}

fn verify_choosing_members(harness: &Harness, prepared: &[PreparedSlot]) -> Outcome {
    let store = harness
        .store
        .lock()
        .map_err(|error| format!("store lock: {error}"))?;
    let project = store.project(1).ok_or("missing project 1")?;
    let members = airc_members(project);
    for slot in prepared {
        let expected = AircMember::new(
            &format!("thread-{}", slot.thread_id),
            AircMemberState::Choosing,
        );
        if !members.contains(&expected) {
            return Err(format!("missing Choosing AIRC member for {}", slot.slot.nick).into());
        }
    }
    println!("AIRC members in Choosing work: {}", prepared.len());
    Ok(())
}

fn pool_for_project(
    project: &ProjectItem,
) -> Result<WorkPool, velocity_threads::work_pool::WorkPoolError> {
    let mut pieces = Vec::new();
    for task in &project.tasks {
        if matches!(task.state, TaskState::Draft | TaskState::Done) {
            continue;
        }
        let owner = (task.state == TaskState::Now)
            .then_some(task.assigned_thread_id)
            .flatten();
        pieces.push(WorkPiece::new(
            WorkKey::Task {
                identifier: task.id,
            },
            task.text.clone(),
            format!("Task #{}", task.id),
            owner,
        ));
    }
    let section = "M9/Goal swarm launch (F16)".to_owned();
    if !project.built_spec_sections.contains(&section) {
        let owner = project
            .threads
            .iter()
            .find(|thread| thread.spec_section.as_ref() == Some(&section))
            .map(|thread| thread.id);
        pieces.push(WorkPiece::new(
            WorkKey::Spec { section },
            "Goal swarm launch".into(),
            "SPEC.md \u{00b7} M9".into(),
            owner,
        ));
    }
    Ok(WorkPool::new(project.id, pieces))
}

fn run_concurrent_claims(
    harness: &Harness,
    prepared: &[PreparedSlot],
) -> Outcome<Vec<(usize, ClaimPhaseOutcome)>> {
    let start_gate = Barrier::new(prepared.len());
    let outcomes: Mutex<Vec<(usize, ClaimPhaseOutcome)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for (index, item) in prepared.iter().enumerate() {
            let start_gate = &start_gate;
            let outcomes = &outcomes;
            scope.spawn(move || -> Result<(), String> {
                start_gate.wait();
                let outcome = choose_and_claim_with(
                    &harness.store,
                    &harness.channel,
                    1,
                    item.thread_id,
                    &item.slot,
                    &item.worktree_path,
                    &item.initial_branch,
                    "10:01",
                    pool_for_project,
                )
                .map_err(|error| error.to_string())?;
                outcomes
                    .lock()
                    .map_err(|error| error.to_string())?
                    .push((index, outcome));
                Ok(())
            });
        }
    });
    let mut collected = outcomes
        .into_inner()
        .map_err(|error| format!("outcomes lock: {error}"))?;
    collected.sort_by_key(|(index, _)| *index);
    if collected.len() != prepared.len() {
        return Err("one or more concurrent claim threads failed".into());
    }
    Ok(collected)
}

fn verify_claims_and_leaves(
    harness: &Harness,
    swarm_id: u32,
    prepared: &[PreparedSlot],
    outcomes: &[(usize, ClaimPhaseOutcome)],
) -> Outcome {
    let mut claimed_keys = Vec::new();
    let mut left_count = 0;
    for (index, outcome) in outcomes {
        let slot = &prepared[*index];
        match outcome {
            ClaimPhaseOutcome::Claimed(work) => {
                verify_single_claim(harness, slot, work)?;
                claimed_keys.push(work.piece.key.claim_piece());
            }
            ClaimPhaseOutcome::Left { left_entry, .. } => {
                left_count += 1;
                if slot.worktree_path.exists() || left_entry.is_none() {
                    return Err(
                        "departed agent must remove untouched worktree and post left".into(),
                    );
                }
                println!(
                    "agent {} found nothing unclaimed -> left and removed {}",
                    slot.slot.nick,
                    slot.worktree_path.display()
                );
            }
        }
    }
    claimed_keys.sort();
    claimed_keys.dedup();
    if claimed_keys.len() != 2 || left_count != 2 {
        return Err(format!(
            "expected 2 distinct claims and 2 leaves, got claims={claimed_keys:?}, left={left_count}"
        )
        .into());
    }
    let store = harness
        .store
        .lock()
        .map_err(|error| format!("store lock: {error}"))?;
    let project = store.project(1).ok_or("missing project 1")?;
    let swarm = project
        .goal_swarms
        .iter()
        .find(|item| item.id == swarm_id)
        .ok_or("missing swarm")?;
    println!("swarm feed after claim phase: {} events", swarm.feed.len());
    Ok(())
}

fn verify_single_claim(harness: &Harness, slot: &PreparedSlot, work: &ClaimedGoalWork) -> Outcome {
    if !slot.worktree_path.ends_with(&slot.slot.nick) || !slot.worktree_path.exists() {
        return Err("claimed agent must keep its <nick> worktree folder".into());
    }
    let head = run_git(&slot.worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if head != work.branch_name || head == slot.initial_branch {
        return Err(format!(
            "expected renamed branch {}, git HEAD is {head}",
            work.branch_name
        )
        .into());
    }
    if !work
        .prompt
        .contains("Implement and run the feature first; then write Lean proofs only if")
    {
        return Err("goal agent prompt is missing Rule 8 implementation-first order (#73)".into());
    }
    println!(
        "agent {} claimed {} -> folder {}, branch {}",
        slot.slot.nick,
        work.piece.key.claim_piece(),
        harness
            .worktree_root
            .join("veloc")
            .join(&slot.slot.nick)
            .display(),
        work.branch_name
    );
    Ok(())
}

fn run_merges_and_keep_going(harness: &Harness, swarm_id: u32) -> Outcome {
    let claimed_threads: Vec<u32> = {
        let mut store = harness
            .store
            .lock()
            .map_err(|error| format!("store lock: {error}"))?;
        store.update_project(1, |project| {
            project.tasks.push(TaskItem::new(
                2,
                "Wire add goal agents".into(),
                TaskState::Todo,
                TaskAttribution::Spec,
            ));
            Ok::<_, velocity_store::projects::ProjectError>(())
        })?;
        store
            .project(1)
            .ok_or("missing project 1")?
            .threads
            .iter()
            .map(|thread| thread.id)
            .collect()
    };
    let (first, second) = (claimed_threads[0], claimed_threads[1]);
    land_merge(harness, first, 101)?;
    let round_two = evaluate_keep_going(
        &harness.store,
        &harness.channel,
        1,
        first,
        "10:15",
        pool_for_project,
    )?;
    let KeepGoingOutcome::NextRound(next_slot) = round_two else {
        return Err(format!("expected NextRound on keep_going, got {round_two:?}").into());
    };
    println!(
        "keep_going after PR #101 merged -> {} continues onto remaining work",
        next_slot.nick
    );
    let next_thread = run_followup_round(harness, &next_slot)?;
    land_merge(harness, second, 102)?;
    land_merge(harness, next_thread, 103)?;
    let final_step = evaluate_keep_going(
        &harness.store,
        &harness.channel,
        1,
        next_thread,
        "10:30",
        pool_for_project,
    )?;
    if !matches!(final_step, KeepGoingOutcome::LeftEmpty { .. }) {
        return Err(
            format!("expected LeftEmpty once pool is exhausted, got {final_step:?}").into(),
        );
    }
    verify_final_state(harness, swarm_id)
}

fn run_followup_round(harness: &Harness, slot: &GoalAgentSlot) -> Outcome<u32> {
    let prepared = create_choosing_worktrees(harness, std::slice::from_ref(slot))?;
    let only = prepared.first().ok_or("missing prepared follow-up slot")?;
    let outcome = choose_and_claim_with(
        &harness.store,
        &harness.channel,
        1,
        only.thread_id,
        &only.slot,
        &only.worktree_path,
        &only.initial_branch,
        "10:16",
        pool_for_project,
    )?;
    if !matches!(outcome, ClaimPhaseOutcome::Claimed(_)) {
        return Err("expected follow-up round to claim Task 2".into());
    }
    Ok(only.thread_id)
}

fn land_merge(harness: &Harness, thread_id: u32, pr_number: u32) -> Outcome {
    let (worktree, branch) = {
        let store = harness
            .store
            .lock()
            .map_err(|error| format!("store lock: {error}"))?;
        let thread = store
            .project(1)
            .and_then(|project| project.threads.iter().find(|item| item.id == thread_id))
            .ok_or("missing thread to merge")?;
        (
            PathBuf::from(thread.worktree_path.clone().ok_or("missing worktree")?),
            thread.branch_name.clone(),
        )
    };
    remove_merged_worktree(&harness.repository, &worktree, &branch, true)?;
    let member = format!("thread-{thread_id}");
    for claim in harness.channel.claims()? {
        if claim.member == member {
            harness.channel.release(&member, claim.key())?;
        }
    }
    let snapshot = merged_snapshot(pr_number, &branch)?;
    let mut watch = FollowThrough::default();
    watch.observe(&snapshot);
    watch.cleaned_up();
    harness
        .store
        .lock()
        .map_err(|error| format!("store lock: {error}"))?
        .save_follow_through(1, thread_id, &snapshot, watch)?;
    Ok(())
}

fn merged_snapshot(number: u32, branch: &str) -> Outcome<PullRequestSnapshot> {
    Ok(serde_json::from_value(serde_json::json!({
        "pull_request": {
            "id": format!("PR_{number}"),
            "number": number,
            "title": format!("Complete {branch}"),
            "body": "",
            "url": format!("https://example.invalid/pr/{number}"),
            "state": "MERGED",
            "isDraft": false,
            "author": null,
            "headRefOid": format!("deadbeef{number}"),
            "headRefName": branch,
            "baseRefName": "master",
            "additions": 42,
            "deletions": 5,
            "createdAt": "2099-01-01T00:00:00Z",
            "updatedAt": "2099-01-01T00:00:00Z",
            "closedAt": "2099-01-01T00:00:00Z",
            "mergedAt": "2099-01-01T00:00:00Z",
            "reviewDecision": "APPROVED",
            "mergeStateStatus": "CLEAN"
        },
        "checks": [],
        "reviews": [],
        "comments": [],
        "review_threads": []
    }))?)
}

fn verify_final_state(harness: &Harness, swarm_id: u32) -> Outcome {
    let store = harness
        .store
        .lock()
        .map_err(|error| format!("store lock: {error}"))?;
    let project = store.project(1).ok_or("missing project 1")?;
    let all_done = project
        .tasks
        .iter()
        .all(|task| task.state == TaskState::Done);
    if !all_done || project.built_spec_sections.is_empty() {
        return Err("expected all tasks Done and SPEC section recorded built".into());
    }
    let left_events = harness
        .channel
        .events(0)?
        .into_iter()
        .filter(|entry| entry.kind == "left")
        .count();
    let swarm = project
        .goal_swarms
        .iter()
        .find(|item| item.id == swarm_id)
        .ok_or("missing goal swarm")?;
    println!(
        "final check: {} tasks Done, {} built spec sections, {} AIRC `left` events, {} feed items",
        project.tasks.len(),
        project.built_spec_sections.len(),
        left_events,
        swarm.feed.len()
    );
    Ok(())
}
