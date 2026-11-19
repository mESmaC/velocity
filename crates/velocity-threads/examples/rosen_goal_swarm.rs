//! #385 smoke run: three members, real Rosen workspaces, claims, a D19 stack, lands,
//! keep-going, restart and a revert. No provider account or production project is used.
//! The git assumptions found were setup, branch rename, empty-member cleanup, prompts,
//! PR-only completion/keep-going, board labels and dependency checkout updates.
//! Run: rosen run -- cargo run -p velocity-threads --example rosen_goal_swarm
//! Add -- --after-land with a Rosen binary supporting rosen#365.
#![allow(clippy::disallowed_methods)]

use std::path::Path;
use std::process::Command;
use std::sync::{Barrier, Mutex};

use velocity_airc::Channel;
use velocity_rosen::{
    AgentWorkspace, ChangeEvent, EventStream, ImportSource, ProjectServer, Servers,
};
use velocity_store::goal_swarm_dialog::GoalMixRow;
use velocity_store::goal_swarm_view::goal_claims_board;
use velocity_store::goal_swarms::{GoalAgentSlot, LaunchGoalSwarmRequest};
use velocity_store::rosen::RosenProgress;
use velocity_store::{
    EffortLevel, PersistedState, ProjectItem, ProviderKind, SpeedMode, StateStore, TaskAttribution,
    TaskItem, TaskState, ThreadState,
};
use velocity_threads::goal_swarm::{
    ClaimPhaseOutcome, KeepGoingOutcome, choose_and_claim_with, evaluate_keep_going,
    plan_goal_member,
};

type Outcome<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Member {
    thread: u32,
    workspace: AgentWorkspace,
    slot: GoalAgentSlot,
    branch: String,
}

fn main() -> Outcome {
    let root = std::env::temp_dir().join(format!("velocity goal 385 {}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root)?;
    seed_repository(&root.join("repo"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(&root))?;
    println!(
        "PASS: three Rosen members, stack, landing, keep-going, restart and revert; {}",
        root.display()
    );
    Ok(())
}

async fn run(root: &Path) -> Outcome {
    let servers = Servers::new(root.join("app"));
    let source = ImportSource::for_checkout(&root.join("repo"), "master");
    let server = servers
        .ready(1, &source, Some(root.join("workspaces/.pool")))
        .await?;
    let result = scenario(root, &server).await;
    servers.shutdown_all().await;
    result
}

async fn scenario(root: &Path, server: &ProjectServer) -> Outcome {
    let store = Mutex::new(seed_store(root)?);
    let channel = Channel::open(&root.join("app"), "1")?;
    let (swarm, mut members) = launch_three(&store, server).await?;
    claim_together(&store, &channel, &members)?;
    store
        .lock()
        .map_err(|e| e.to_string())?
        .update_thread(1, members[0].thread, |thread| {
            thread.issue = Some(velocity_store::issues::ThreadIssue::new(
                "smoke/local".into(),
                385,
            ));
        })?;
    stack_member(&store, server, &channel, &mut members).await?;
    let mut events = EventStream::open(server, 0).await?;
    for member in &members {
        name_and_land(server, member).await?;
        take_land(&store, &mut events, &member.workspace.change).await?;
    }
    let first = &members[0];
    for claim in channel
        .claims()?
        .into_iter()
        .filter(|claim| claim.member == format!("thread-{}", first.thread))
    {
        channel.release(&claim.member, claim.key())?;
    }
    let next = evaluate_keep_going(
        &store,
        &channel,
        1,
        first.thread,
        "10:05",
        velocity_threads::work_pool::read_work_pool,
    )?;
    let KeepGoingOutcome::NextRound(slot) = next else {
        return Err("Keep going did not advance".into());
    };
    let next = prepare(&store, server, slot).await?;
    assert_ne!(next.workspace.root, first.workspace.root);
    claim_together(&store, &channel, std::slice::from_ref(&next))?;
    name_and_land(server, &next).await?;
    take_land(&store, &mut events, &next.workspace.change).await?;
    velocity_store::save_pending(&store)?;
    verify_restart_and_revert(root, swarm, first)?;
    Ok(())
}

async fn launch_three(
    store: &Mutex<StateStore>,
    server: &ProjectServer,
) -> Outcome<(u32, Vec<Member>)> {
    let request = LaunchGoalSwarmRequest::new(
        1,
        "Build the claimed task.".into(),
        vec![GoalMixRow::new(
            ProviderKind::Codex,
            EffortLevel::High,
            SpeedMode::Regular,
            3,
        )],
        true,
    );
    let (swarm, slots) = store
        .lock()
        .map_err(|e| e.to_string())?
        .open_goal_swarm(&request, "10:00".into())?;
    let mut members = Vec::new();
    for slot in slots {
        members.push(prepare(store, server, slot).await?);
    }
    Ok((swarm, members))
}

async fn prepare(
    store: &Mutex<StateStore>,
    server: &ProjectServer,
    slot: GoalAgentSlot,
) -> Outcome<Member> {
    let (plan, thread) = {
        let mut store = store.lock().map_err(|e| e.to_string())?;
        let project = store.project(1).ok_or("Missing project")?;
        let plan = plan_goal_member(
            project,
            &store.worktree_context().preferences,
            &store.settings(),
            &slot,
        )?;
        let thread = store.start_goal_member_thread(1, &slot, plan.thread.clone())?;
        (plan, thread)
    };
    let root = &plan.worktree.worktree_path;
    let workspace =
        velocity_rosen::create_workspace(server, &velocity_rosen::agent_name(root), root, None)
            .await?;
    store
        .lock()
        .map_err(|e| e.to_string())?
        .update_thread(1, thread, |saved| {
            saved.rosen_change = Some(workspace.change.clone());
        })?;
    println!("{}: {} ({})", slot.nick, workspace.change, workspace.method);
    Ok(Member {
        thread,
        workspace,
        slot,
        branch: plan.thread.branch_name,
    })
}

fn claim_together(store: &Mutex<StateStore>, channel: &Channel, members: &[Member]) -> Outcome {
    let barrier = Barrier::new(members.len());
    std::thread::scope(|scope| -> Outcome {
        let mut workers = Vec::new();
        for member in members {
            let barrier = &barrier;
            workers.push(scope.spawn(move || -> Result<(), String> {
                barrier.wait();
                // The first unclaimed piece, as keep-going below picks it. Since #314 the
                // goal-prompt chooser starts the member's real agent; this smoke covers Rosen
                // stacking, landing, restart and revert, and must run where no agent is signed in.
                let outcome = choose_and_claim_with(
                    store,
                    channel,
                    1,
                    member.thread,
                    &member.slot,
                    &member.workspace.root,
                    &member.branch,
                    "10:01",
                    velocity_threads::work_pool::read_work_pool,
                )
                .map_err(|e| e.to_string())?;
                let ClaimPhaseOutcome::Claimed(work) = outcome else {
                    return Err("Member left before claiming".into());
                };
                if work.branch_name != member.branch
                    || !work.prompt.contains("rosen commit")
                    || work.prompt.contains("open a PR")
                {
                    return Err("Git lifecycle leaked into a Rosen member".into());
                }
                std::fs::write(
                    member
                        .workspace
                        .root
                        .join(format!("task-{}.rs", member.thread)),
                    "// implemented\n",
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            }));
        }
        for worker in workers {
            worker.join().map_err(|_| "Claim worker panicked")??;
        }
        Ok(())
    })
}

async fn stack_member(
    store: &Mutex<StateStore>,
    server: &ProjectServer,
    channel: &Channel,
    members: &mut [Member],
) -> Outcome {
    cli(server, &members[0], &["commit", "-m", "Prepare dependency"])?;
    let parent = members[0].workspace.change.clone();
    let original = &members[1].workspace;
    let root = original
        .root
        .parent()
        .ok_or("Missing workspace parent")?
        .join(".stacks")
        .join(&parent)
        .join(original.root.file_name().ok_or("Missing workspace name")?);
    let workspace = velocity_rosen::stack_workspace(
        server,
        &velocity_rosen::agent_name(&root),
        &original.root,
        &parent,
        &root,
    )
    .await?;
    assert!(
        root.join(format!("task-{}.rs", members[1].thread))
            .is_file()
    );
    assert!(root.join(".rosen").exists() && !root.join(".git").exists());
    store.lock().map_err(|e| e.to_string())?.defer_saves();
    store
        .lock()
        .map_err(|e| e.to_string())?
        .update_thread(1, members[1].thread, |saved| {
            saved.rosen_change = Some(workspace.change.clone());
            saved.worktree_path = Some(root.display().to_string());
            saved.goal_stack = Some(velocity_store::goal_rosen::GoalStack::new(
                parent.clone(),
                original.change.clone(),
                original.root.display().to_string(),
            ));
        })?;
    // The source must survive a failed save; production applies this barrier on retries too.
    velocity_store::save_pending(store)?;
    velocity_rosen::abandon_change(
        server,
        &velocity_rosen::agent_name(&original.root),
        &original.change,
    )
    .await?;
    members[1].workspace = workspace;
    assert_eq!(channel.claims()?.len(), 3);
    println!("stacked member on {parent}; task claim and edits retained");
    Ok(())
}

async fn name_and_land(server: &ProjectServer, member: &Member) -> Outcome {
    cli(server, member, &["commit", "-m", "Implement goal task"])?;
    let outcome =
        velocity_rosen::landing::land(server, "smoke-lander", &member.workspace.change).await?;
    if !matches!(outcome, velocity_rosen::landing::LandOutcome::Landed { .. }) {
        return Err(format!("Land failed: {outcome:?}").into());
    }
    Ok(())
}

async fn take_land(store: &Mutex<StateStore>, events: &mut EventStream, change: &str) -> Outcome {
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(20), events.next())
            .await?
            .ok_or("Event stream ended")?;
        let Some((identifier, meaning)) = velocity_rosen::change_event(&event) else {
            continue;
        };
        let progress = match meaning {
            ChangeEvent::Named => RosenProgress::Named,
            ChangeEvent::Landed => RosenProgress::Landed,
            _ => continue,
        };
        store
            .lock()
            .map_err(|e| e.to_string())?
            .save_rosen_progress(1, event.seq, identifier, (progress, ""))?;
        if identifier == change && meaning == ChangeEvent::Landed {
            return Ok(());
        }
    }
}

fn verify_restart_and_revert(root: &Path, swarm: u32, member: &Member) -> Outcome {
    let mut saved = StateStore::load_or_default(root.join("state"))?;
    let project = saved.project(1).ok_or("Missing reloaded project")?;
    verify_landed_board(project, swarm)?;
    let sequence = saved.rosen_event_cursor(1) + 1;
    saved.save_rosen_progress(
        1,
        sequence,
        &member.workspace.change,
        (RosenProgress::Reverted, "build broke"),
    )?;
    let reloaded = StateStore::load_or_default(root.join("state"))?;
    let project = reloaded.project(1).ok_or("Missing reverted project")?;
    assert!(project.completed_goal_issues.is_empty());
    let pool = velocity_threads::work_pool::read_work_pool(project)?;
    assert!(pool.pieces.iter().any(|piece| {
        matches!(
            piece.key,
            velocity_core::work_pool::WorkKey::Issue { number: 385, .. }
        ) && piece.thread_identifier == Some(member.thread)
    }));
    assert!(
        project
            .tasks
            .iter()
            .any(|task| task.state == TaskState::Now
                && task.assigned_thread_id == Some(member.thread))
    );
    assert!(!reloaded.rosen_follow_ups(1).is_empty());
    Ok(())
}

fn verify_landed_board(project: &ProjectItem, swarm: u32) -> Outcome {
    assert_eq!(project.completed_goal_issues.len(), 1);
    assert!(
        project
            .tasks
            .iter()
            .all(|task| task.state == TaskState::Done)
    );
    assert!(
        project
            .threads
            .iter()
            .all(|thread| thread.state == ThreadState::Merged)
    );
    let pool = velocity_threads::work_pool::read_work_pool(project)?;
    let board = goal_claims_board(project, swarm, &pool).ok_or("Missing board")?;
    assert_eq!(board.rows.len(), 4);
    assert!(
        project
            .threads
            .iter()
            .any(|thread| thread.goal_stack.is_some())
    );
    assert!(
        board
            .rows
            .iter()
            .all(|row| row.status.starts_with("landed "))
    );
    Ok(())
}

fn seed_store(root: &Path) -> Outcome<StateStore> {
    let mut state = PersistedState::default();
    state.settings.worktrees.location_root = Some(root.join("workspaces").display().to_string());
    state.settings.worktrees.copy_files.clear();
    state.settings.worktrees.setup_command.clear();
    let mut project = ProjectItem::new(
        1,
        "goal-smoke".into(),
        root.join("repo").display().to_string(),
    );
    project.spec_locked = true;
    for id in 1..=4 {
        project.tasks.push(TaskItem::new(
            id,
            format!("Build task {id}"),
            TaskState::Todo,
            TaskAttribution::Spec,
        ));
    }
    state.projects.push(project);
    std::fs::create_dir_all(root.join("state"))?;
    std::fs::write(root.join("state/state.json"), serde_json::to_vec(&state)?)?;
    Ok(StateStore::load_or_default(root.join("state"))?)
}

fn seed_repository(repository: &Path) -> Outcome {
    std::fs::create_dir_all(repository)?;
    for args in [
        vec!["init", "-b", "master"],
        vec!["config", "user.name", "Goal Smoke"],
        vec!["config", "user.email", "smoke@example.invalid"],
    ] {
        command(Command::new("git").current_dir(repository).args(args))?;
    }
    std::fs::write(
        repository.join("rosen.toml"),
        "review = \"after-land\"\n[warm_pool]\nsize = 1\n",
    )?;
    std::fs::write(
        repository.join("SPEC.md"),
        "# Goal smoke\nAll work is on the Task List.\n",
    )?;
    command(
        Command::new("git")
            .current_dir(repository)
            .args(["add", "."]),
    )?;
    command(Command::new("git").current_dir(repository).args([
        "commit",
        "-m",
        "Seed smoke project",
    ]))?;
    Ok(())
}

fn cli(server: &ProjectServer, member: &Member, arguments: &[&str]) -> Outcome {
    command(
        Command::new("rosen")
            .current_dir(&member.workspace.root)
            .args(arguments)
            .envs(velocity_rosen::agent_environment(
                server,
                &velocity_rosen::agent_name(&member.workspace.root),
                &member.workspace.change,
            )),
    )
}

fn command(command: &mut Command) -> Outcome {
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "{command:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}
