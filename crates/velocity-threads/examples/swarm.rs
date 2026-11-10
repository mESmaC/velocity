//! F10 without a window: seed a throwaway repository and project, open the Swarm dialog,
//! launch a swarm through the real store, planner and `git worktree` setup (no agents), then
//! replay a real merged PR through the F6 save path and check the targeted rebase overlap.
//!
//! cargo run -p velocity-threads --example swarm -- <empty scratch folder> <github checkout> <merged PR>
use std::path::{Path, PathBuf};
use std::process::Command;

use velocity_core::layout_state::LayoutState;
use velocity_core::swarm::{PullRequestStep, SwarmSource, pull_request_event};
use velocity_github::PullRequestClient;
use velocity_github::follow_through::FollowThrough;
use velocity_store::swarm_dialog::{OpenIssues, SwarmDialogQuery};
use velocity_store::swarms::{LaunchSwarmRequest, SwarmItem};
use velocity_store::{
    PersistedState, ProjectItem, ProviderKind, StateStore, TaskAttribution, TaskItem, TaskState,
    ThreadState,
};
use velocity_threads::swarm::{plan_swarm_member, require_swarm_launch};

type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> Outcome<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let scratch = PathBuf::from(arguments.first().ok_or("missing <scratch folder>")?);
    let checkout = PathBuf::from(arguments.get(1).ok_or("missing <github checkout>")?);
    let merged: u32 = arguments.get(2).ok_or("missing <merged PR>")?.parse()?;
    let repository = seed_repository(&scratch)?;
    let state = scratch.join("state");
    seed_state(&state, &repository, &scratch.join("worktrees"))?;
    let mut store = StateStore::load_or_default(&state)?;
    show_dialog(&store, "dialog before launch")?;
    let members = launch(&mut store)?;
    show_layout(&store, "after launch")?;
    // Member 1 edits a file the merged PR also changed, and one it didn't.
    std::fs::write(members[0].1.join("SPEC.md"), "member 1 edit\n")?;
    std::fs::write(members[0].1.join("README.md"), "member 1 edit\n")?;
    store.update_thread(1, members[1].0, |thread| {
        thread.state = ThreadState::Review;
        thread.pull_request_number = Some(12);
        thread.review_stage = Some(velocity_store::ReviewStage::Fixing);
    })?;
    merge_member(&mut store, &checkout, merged, members[2].0)?;
    rebase_overlap(&checkout, merged, &members[0].1)?;
    show_layout(&store, "after a fixing PR and a merge")?;
    show_dialog(&store, "dialog after launch")?;
    store.remove_thread(1, members[1].0)?;
    let reloaded = StateStore::load_or_default(&state)?;
    show_layout(
        &reloaded,
        "after deleting member 2 and reloading state.json",
    )?;
    let tasks: Vec<_> = reloaded.state().projects[0]
        .tasks
        .iter()
        .map(|task| format!("{}={:?}", task.id, task.state))
        .collect();
    println!("tasks: {}", tasks.join(" "));
    Ok(())
}

fn git(folder: &Path, arguments: &[&str]) -> Outcome<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(arguments)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git {arguments:?} failed").into())
    }
}

fn seed_repository(scratch: &Path) -> Outcome<PathBuf> {
    let repository = scratch.join("repo with spaces");
    std::fs::create_dir_all(&repository)?;
    git(&repository, &["init", "-q", "-b", "master"])?;
    std::fs::write(repository.join("README.md"), "swarm example\n")?;
    git(&repository, &["add", "README.md"])?;
    git(
        &repository,
        &[
            "-c",
            "user.name=velocity",
            "-c",
            "user.email=velocity@localhost",
            "commit",
            "-qm",
            "init",
        ],
    )?;
    Ok(repository)
}

fn task(id: u32, text: &str, state: TaskState) -> TaskItem {
    TaskItem::new(id, text.into(), state, TaskAttribution::Spec)
}

fn seed_state(state: &Path, repository: &Path, worktrees: &Path) -> Outcome<()> {
    let mut persisted = PersistedState::default();
    persisted.settings.worktrees.location_root = Some(worktrees.display().to_string());
    persisted.settings.worktrees.setup_command = String::new();
    persisted.settings.worktrees.copy_files = String::new();
    let mut project = ProjectItem::new(1, "Swarm example".into(), repository.display().to_string());
    project.spec_locked = true;
    project.tasks = vec![
        task(1, "Add a CSV export to reports", TaskState::Todo),
        task(2, "Fix the login timeout", TaskState::Todo),
        task(3, "Write the changelog", TaskState::Todo),
        task(4, "Set up CI", TaskState::Done),
    ];
    persisted.projects.push(project);
    std::fs::create_dir_all(state)?;
    std::fs::write(
        state.join("state.json"),
        serde_json::to_vec_pretty(&persisted)?,
    )?;
    Ok(())
}

fn show_dialog(store: &StateStore, heading: &str) -> Outcome<()> {
    let query = SwarmDialogQuery {
        source: SwarmSource::Tasks,
        items: None,
        providers: vec![ProviderKind::Claude, ProviderKind::Codex],
        into: None,
    };
    let issues = OpenIssues {
        rows: &[],
        loaded: true,
    };
    let dialog = store.swarm_dialog(&query, issues).ok_or("no project")?;
    println!("\n== {heading}");
    println!(
        "source counts: tasks {} · issues {}",
        dialog.task_count, dialog.issue_count
    );
    for item in &dialog.items {
        let mark = if item.selected {
            "[x]"
        } else if item.selectable {
            "[ ]"
        } else {
            "[-]"
        };
        println!("  {mark} {} · {}", item.title, item.note);
    }
    let models: Vec<_> = dialog
        .models
        .iter()
        .map(|model| {
            format!(
                "{}{}",
                model.model_name,
                if model.selected { "*" } else { "" }
            )
        })
        .collect();
    println!("models: {}", models.join(", "));
    println!("{}", dialog.split_label);
    println!(
        "PR duty: {:?} (add {})",
        dialog.pr_duty_label, dialog.pr_duty_shortfall
    );
    println!("footer: {} · {}", dialog.summary, dialog.launch_label);
    Ok(())
}

fn launch(store: &mut StateStore) -> Outcome<Vec<(u32, PathBuf)>> {
    let request = LaunchSwarmRequest {
        project_identifier: 1,
        source: SwarmSource::Tasks,
        items: vec![SwarmItem::Task(1), SwarmItem::Task(2), SwarmItem::Task(3)],
        providers: vec![ProviderKind::Claude, ProviderKind::Codex],
        close_issues: true,
        rebase_on_merge: true,
        add_pr_duty: false,
        into: None,
    };
    require_swarm_launch(store, &request)?;
    let swarm = store.open_swarm(&request, "09:41".into())?;
    let assignments = velocity_core::swarm::round_robin(request.items.len(), 2);
    let mut members = Vec::new();
    for (item, provider) in request.items.iter().zip(assignments) {
        let provider = request.providers[provider];
        let plan = plan_swarm_member(store, &request, "", &[], *item, provider)?;
        let thread =
            store.add_swarm_member(1, swarm, plan.launch.thread.clone(), plan.item, None)?;
        let created = velocity_git::execute_worktree_setup(&plan.launch.worktree)?;
        println!(
            "member {thread}: {:?} · {} · {} · {}",
            provider,
            plan.launch.thread.model_name,
            plan.launch.thread.branch_name,
            created.transcript_step
        );
        members.push((thread, plan.launch.worktree.worktree_path.clone()));
    }
    let again = plan_swarm_member(
        store,
        &request,
        "",
        &[],
        SwarmItem::Task(1),
        ProviderKind::Claude,
    );
    println!(
        "relaunching task 1 is refused: {:?}",
        again.err().map(|error| error.to_string())
    );
    Ok(members)
}

fn show_layout(store: &StateStore, heading: &str) -> Outcome<()> {
    let mut layout = LayoutState::default();
    store.restore_project_layout(&mut layout);
    let snapshot = layout.snapshot();
    println!("\n== layout {heading} · view {:?}", snapshot.view);
    for group in &snapshot.groups {
        println!("[{}] {}", group.title, group.count);
        for row in &group.rows {
            let indent = if row.member { "    " } else { "  " };
            println!(
                "{indent}{} ×{} · {} · {:?}",
                row.title, row.agents, row.metadata, row.segments
            );
        }
    }
    let swarm = snapshot
        .active_swarm
        .ok_or("the swarm is not the selected view")?;
    println!(
        "{} · {} · {} · {:?}",
        swarm.title, swarm.agents_label, swarm.started_label, swarm.merged_label
    );
    for card in &swarm.cards {
        println!(
            "  card {} [{}] {:?} · {} · {}",
            card.title, card.reference, card.track, card.doing, card.agent_label
        );
    }
    let legend: Vec<_> = swarm.legend.iter().map(|item| item.label.clone()).collect();
    println!("  legend: {}", legend.join(" · "));
    Ok(())
}

fn merge_member(store: &mut StateStore, checkout: &Path, number: u32, thread: u32) -> Outcome<()> {
    let snapshot = PullRequestClient::discover(checkout)?.snapshot(number)?;
    store.update_thread(1, thread, |record| {
        record.branch_name = snapshot.pull_request.head_ref_name.clone();
    })?;
    let mut watch = FollowThrough::default();
    watch.observe(&snapshot);
    watch.cleaned_up();
    store.save_follow_through(1, thread, &snapshot, watch.clone())?;
    let after = PullRequestStep {
        number,
        stage: &watch.stage,
        worktree_deleted: watch.worktree_deleted,
    };
    println!(
        "\nfeed line for the merge: {:?}",
        pull_request_event(None, after, "task done")
    );
    Ok(())
}

fn rebase_overlap(checkout: &Path, number: u32, worktree: &Path) -> Outcome<()> {
    let merged = PullRequestClient::discover(checkout)?.changed_files(number)?;
    let changed = velocity_git::worktree_changed_files(worktree, "master")?;
    let overlap: Vec<_> = changed
        .iter()
        .filter(|file| merged.contains(file))
        .collect();
    println!("PR #{number} files: {merged:?}");
    println!("member 1 changed: {changed:?} → rebase notice for {overlap:?}");
    Ok(())
}
