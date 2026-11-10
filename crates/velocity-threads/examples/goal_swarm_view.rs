//! Headless check of the M9 Goal swarm view and sidebar row (`SPEC.md` F16).
//!
//! Launches a goal swarm in a scratch `state.json`, moves its agents through choosing,
//! claimed, waiting, in review and merged, then prints what the window would draw: the
//! sidebar row with its nested members, the view's header, segment bar, cards and feed,
//! and the claims board. + Agents must open the dialog on the swarm's own prompt.
//!
//! `pwsh -NoProfile -File scripts/build-queue.ps1 cargo run -p velocity-threads --example goal_swarm_view`

use std::path::PathBuf;

use velocity_core::goal_swarm_view::BoardRowState;
use velocity_core::layout::ThreadGroup;
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutSnapshot};
use velocity_core::swarm::SwarmStage;
use velocity_core::work_pool::{WorkKey, WorkPiece, WorkPool};
use velocity_store::goal_swarm_dialog::GoalMixRow;
use velocity_store::goal_swarm_view::goal_claims_board;
use velocity_store::goal_swarms::LaunchGoalSwarmRequest;
use velocity_store::{
    EffortLevel, PersistedState, ProjectItem, ProviderKind, ReviewStage, SpeedMode, StateStore,
    TaskAttribution, TaskItem, TaskState, ThreadState,
};
use velocity_threads::goal_swarm::plan_goal_member;

type Outcome<Value = ()> = Result<Value, Box<dyn std::error::Error>>;

fn main() -> Outcome {
    let root = scratch_root()?;
    let result = run(&root);
    let _ = std::fs::remove_dir_all(&root);
    result
}

fn run(root: &std::path::Path) -> Outcome {
    let mut store = seeded_store(root)?;
    let (swarm, threads) = launch(&mut store)?;
    move_agents(&mut store, &threads)?;
    let mut layout = LayoutState::default();
    store.refresh_layout(&mut layout);
    layout.apply(LayoutAction::OpenThread(swarm as usize));
    let snapshot = layout.snapshot();
    check_sidebar(&snapshot, swarm)?;
    check_view(&snapshot)?;
    check_board(&store, swarm, &threads)?;
    layout.apply(LayoutAction::AddGoalAgents(swarm as usize));
    let dialog = layout
        .snapshot()
        .goal_dialog
        .ok_or("+ Agents opened no dialog")?;
    println!(
        "+ Agents → dialog into {:?} on \"{}\"",
        dialog.into, dialog.prompt
    );
    ensure(
        dialog.into == Some(swarm as usize),
        "+ Agents targets the swarm",
    )?;
    println!("goal_swarm_view example: all F16 view assertions passed.");
    Ok(())
}

fn scratch_root() -> Outcome<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("velocity goal view {stamp}"));
    std::fs::create_dir_all(root.join("repo"))?;
    std::fs::create_dir_all(root.join("state"))?;
    Ok(root)
}

fn seeded_store(root: &std::path::Path) -> Outcome<StateStore> {
    let mut persisted = PersistedState::default();
    persisted.settings.worktrees.location_root = Some(root.join("wt").display().to_string());
    let mut project = ProjectItem::new(1, "veloc".into(), root.join("repo").display().to_string());
    project.spec_locked = true;
    for (id, text) in [(1, "Claims board"), (2, "Agent cards")] {
        project.tasks.push(TaskItem::new(
            id,
            text.into(),
            TaskState::Todo,
            TaskAttribution::Spec,
        ));
    }
    persisted.projects.push(project);
    let state = root.join("state");
    std::fs::write(
        state.join("state.json"),
        serde_json::to_string_pretty(&persisted)?,
    )?;
    Ok(StateStore::load_or_default(&state)?)
}

/// Four agents, as `launch_goal_swarm` starts them: 3 on Claude, 1 on Codex Fast.
fn launch(store: &mut StateStore) -> Outcome<(u32, Vec<u32>)> {
    let request = LaunchGoalSwarmRequest::new(
        1,
        "Read AGENTS.md and start dev.".into(),
        vec![
            GoalMixRow::new(
                ProviderKind::Claude,
                EffortLevel::High,
                SpeedMode::Regular,
                3,
            ),
            GoalMixRow::new(ProviderKind::Codex, EffortLevel::Medium, SpeedMode::Fast, 1),
        ],
        false,
        false,
    );
    let (swarm, slots) = store.open_goal_swarm(&request, "10:00".into())?;
    let mut threads = Vec::new();
    for slot in &slots {
        let project = store.project(1).cloned().ok_or("missing project")?;
        let preferences = store.worktree_context().preferences;
        let plan = plan_goal_member(&project, &preferences, &store.settings(), slot)?;
        threads.push(store.start_goal_member_thread(1, slot, plan.thread)?);
    }
    Ok((swarm, threads))
}

/// Agent 1 has PR #41 waiting on CI, agent 2 waits on it, agent 3 merged #40 and agent 4
/// is still choosing.
fn move_agents(store: &mut StateStore, threads: &[u32]) -> Outcome {
    let task = |id| {
        WorkPiece::new(
            WorkKey::Task { identifier: id },
            String::new(),
            String::new(),
            None,
        )
    };
    let issue = WorkPiece::new(
        WorkKey::Issue {
            repository: "o/r".into(),
            number: 7,
        },
        "Fix the composer".into(),
        "#7".into(),
        None,
    );
    let mut first = task(1);
    first.title = "Claims board".into();
    store.claim_goal_piece(1, threads[0], &first, "vel/claims-board".into())?;
    store.claim_goal_piece(1, threads[1], &issue, "vel/issue-7".into())?;
    let mut second = task(2);
    second.title = "Agent cards".into();
    store.claim_goal_piece(1, threads[2], &second, "vel/agent-cards".into())?;
    store.update_project(1, |project| {
        for thread in &mut project.threads {
            if thread.id == threads[0] {
                thread.state = ThreadState::Review;
                thread.review_stage = Some(ReviewStage::ContinuousIntegration);
                thread.pull_request_number = Some(41);
            } else if thread.id == threads[1] {
                thread.dependency_wait =
                    Some("waiting for opus-1 · #41 · shares ui/src/App.svelte".into());
            } else if thread.id == threads[2] {
                thread.state = ThreadState::Merged;
                thread.pull_request_number = Some(40);
            }
        }
        Ok::<_, velocity_store::projects::ProjectError>(())
    })?;
    store.prepare_keep_going(1, threads[2])?;
    Ok(())
}

fn check_sidebar(snapshot: &LayoutSnapshot, swarm: u32) -> Outcome {
    let group = snapshot
        .groups
        .iter()
        .find(|group| group.group == ThreadGroup::Swarm)
        .ok_or("no Swarms group")?;
    for row in &group.rows {
        let bar: Vec<_> = row.segments.iter().map(|stage| stage.label()).collect();
        let lead = if row.member {
            "    "
        } else if row.goal {
            "◎ "
        } else {
            "  "
        };
        println!(
            "{lead}{} ×{} · {} {bar:?}",
            row.title, row.agents, row.metadata
        );
    }
    let row = group.rows.first().ok_or("no goal row")?;
    ensure(
        row.goal && row.identifier == swarm as usize,
        "goal row with target icon",
    )?;
    ensure(
        row.title == "Goal · Read AGENTS.md and start dev",
        "row title",
    )?;
    ensure(row.agents == 4, "×4 agents")?;
    ensure(
        row.segments
            == [
                SwarmStage::PullRequest,
                SwarmStage::Working,
                SwarmStage::Merged,
                SwarmStage::Choosing,
            ],
        "one segment per agent",
    )?;
    let members: Vec<_> = group.rows.iter().filter(|row| row.member).collect();
    ensure(members.len() == 4, "members nest under the goal row")?;
    ensure(
        members[0].title == "Claims board",
        "members titled by their piece",
    )
}

fn check_view(snapshot: &LayoutSnapshot) -> Outcome {
    let view = snapshot
        .active_goal_swarm
        .as_ref()
        .ok_or("no goal swarm view")?;
    println!(
        "\n{} · {} · {} · {:?}",
        view.title, view.agents_label, view.started_label, view.merged_label
    );
    println!("“{}” — {}", view.prompt, view.prompt_label);
    let legend: Vec<_> = view.legend.iter().map(|item| item.label.as_str()).collect();
    println!("legend {legend:?} {:?}", view.waiting_label);
    for card in &view.cards {
        println!(
            "  [{}] {} · {} · {} · {:?} · {}",
            card.nick,
            card.model_label,
            card.title,
            card.stage.label(),
            card.track,
            card.doing
        );
    }
    for event in &view.feed {
        println!("  {} {} {}", event.timestamp, event.actor, event.message);
    }
    ensure(
        view.agents_label == "4 agents · one prompt",
        "header agents",
    )?;
    ensure(
        view.merged_label.as_deref() == Some("1 PR merged"),
        "PRs merged",
    )?;
    ensure(
        view.waiting_label.as_deref() == Some("1 waiting on another agent's PR"),
        "waits",
    )?;
    ensure(view.cards.len() == 4, "one card per agent")?;
    ensure(view.cards[3].title == "choosing work…", "choosing card")?;
    ensure(
        view.cards[1].doing.starts_with("waiting for opus-1"),
        "waiting card",
    )?;
    ensure(
        view.cards[0].doing == "PR #41 · waiting on CI",
        "review card",
    )
}

fn check_board(store: &StateStore, swarm: u32, threads: &[u32]) -> Outcome {
    let piece = |key, title: &str, owner| WorkPiece::new(key, title.into(), String::new(), owner);
    let pool = WorkPool::new(
        1,
        vec![
            piece(
                WorkKey::Issue {
                    repository: "o/r".into(),
                    number: 9,
                },
                "Thread scrolling",
                None,
            ),
            piece(
                WorkKey::Task { identifier: 1 },
                "Claims board",
                Some(threads[0]),
            ),
            piece(
                WorkKey::Issue {
                    repository: "o/r".into(),
                    number: 7,
                },
                "Fix the composer",
                Some(threads[1]),
            ),
            piece(
                WorkKey::Spec {
                    section: "M10/Parking".into(),
                },
                "Parking",
                None,
            ),
        ],
    );
    let project = store.project(1).ok_or("missing project")?;
    let board = goal_claims_board(project, swarm, &pool).ok_or("no board")?;
    println!("\n{}", board.heading);
    for row in &board.rows {
        println!(
            "  {:<8} {:<18} {:<7} {:<8} {}",
            format!("{:?}", row.state),
            row.title,
            row.reference,
            row.holder,
            row.status
        );
    }
    let states: Vec<_> = board.rows.iter().map(|row| row.state).collect();
    use BoardRowState::{Claimed, Merged, Open, Waiting};
    ensure(
        states == [Claimed, Waiting, Open, Open, Merged],
        "claimed, then open, then merged",
    )?;
    ensure(
        board.heading.starts_with("Claims · 2 claimed · 2 open"),
        "board heading",
    )?;
    ensure(
        board.rows[0].status == "#41 · waiting on CI",
        "holder state",
    )?;
    ensure(board.rows[4].status == "merged #40", "merged row")?;
    ensure(
        TaskState::Now == project.tasks[0].state,
        "claim moved the task to now",
    )
}

fn ensure(condition: bool, what: &str) -> Outcome {
    if condition {
        Ok(())
    } else {
        Err(format!("assertion failed: {what}").into())
    }
}
