//! Headless check of the warm worktree pool (SPEC.md F5 Warm pool) against a scratch
//! repository with a GitHub-like `origin`: the pool fills to "Keep N worktrees ready" from one
//! clone, prebuilds each slot, hands one to a thread (moved, renamed for its title, branched,
//! set up at its final path, still prebuilt), catches up after `master` moves, and shrinks
//! when the setting drops.
//!
//! `pwsh -NoProfile -File scripts/build-queue.ps1 cargo run -p velocity-threads --example ready_pool`

// A console example: its children share its console, so no hidden_command (#232).
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use velocity_core::ready_pool::SlotState;
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_store::{PersistedState, ProjectItem, StateStore};
use velocity_threads::{
    PoolHost, PoolProject, ReadyPools, plan_launch_with_preferences, title_worktree_name,
};

type Outcome<Value = ()> = Result<Value, Box<dyn std::error::Error>>;

struct Host(Mutex<StateStore>);

impl PoolHost for Host {
    fn projects(&self) -> Vec<(PoolProject, WorktreePreferences)> {
        let Ok(store) = self.0.lock() else {
            return Vec::new();
        };
        store
            .state()
            .projects
            .iter()
            .filter(|project| project.spec_locked)
            .filter_map(|project| {
                let preferences = store.worktree_preferences(project);
                Some((PoolProject::new(project, &preferences).ok()?, preferences))
            })
            .collect()
    }

    fn changed(&self) {}
}

fn main() -> Outcome {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("velocity-ready-pool-{stamp}"));
    let result = run(&root);
    let _ = std::fs::remove_dir_all(&root);
    result
}

fn run(root: &Path) -> Outcome {
    let repository = scratch_repository(root)?;
    let host = Arc::new(Host(Mutex::new(scratch_store(root, &repository)?)));
    let pools = ReadyPools::new();
    pools.start(Arc::clone(&host))?;

    let started = Instant::now();
    wait_for(&pools, "two ready, prebuilt worktrees", |rows| {
        rows.len() == 2
            && rows
                .iter()
                .all(|state| matches!(state, SlotState::Ready { built: true, .. }))
    })?;
    println!("pool filled and prebuilt in {:?}", started.elapsed());

    let worktree = take_for_thread(&host, &pools)?;
    check_thread_worktree(&worktree)?;

    let tip = advance_master(root, &repository)?;
    pools.wake(Some(1));
    wait_for(&pools, "the refilled pool at the new tip", |rows| {
        rows.len() == 2
            && rows.iter().all(
                |state| matches!(state, SlotState::Ready { base, built: true } if *base == tip),
            )
    })?;
    println!("pool refilled and caught up to {}", &tip[..7]);

    check_guards(&repository, &worktree)?;

    {
        let mut store = host.0.lock().map_err(|_| "store lock")?;
        let mut settings = store.settings();
        settings.worktrees.warm_pool_count = 1;
        store.set_settings(settings)?;
    }
    pools.wake(None);
    wait_for(&pools, "the pool shrunk to one", |rows| rows.len() == 1)?;
    let left = velocity_git::scan_ready_slots(&repository, &root.join("wt").join("pooltest"))?;
    assert_eq!(left.len(), 1, "one slot left on disk");
    assert!(
        left[0]
            .mark
            .as_ref()
            .is_some_and(|mark| mark.built && mark.base == tip)
    );
    println!("ready_pool example: every F5 warm pool check passed");
    Ok(())
}

/// A thread launch as the app runs it: take a slot, check out under the message slug, rename
/// for the title, run the setup command at the final path.
fn take_for_thread(host: &Host, pools: &ReadyPools) -> Outcome<PathBuf> {
    let plan = {
        let store = host.0.lock().map_err(|_| "store lock")?;
        let project = store.project(1).ok_or("project")?.clone();
        let preferences = store.worktree_preferences(&project);
        plan_launch_with_preferences(&project, &preferences, &store.settings(), "add the parser")?
    };
    let name = title_worktree_name(&plan.naming, &plan.worktree, "Warm pool parser")
        .ok_or("title name")?;
    let started = Instant::now();
    let ticket = pools.take(1).ok_or("no ready worktree to take")?;
    let titled =
        velocity_git::execute_titled_pooled_setup(&plan.worktree, Some(&ticket.ready), || {
            Some(name.clone())
        });
    drop(ticket);
    let created = titled.setup?;
    println!(
        "thread worktree ready in {:?} at {}",
        started.elapsed(),
        created.worktree_path.display()
    );
    assert_eq!(created.worktree_path, name.worktree_path);
    assert_eq!(
        titled.branch_name.as_deref(),
        Some(name.branch_name.as_str())
    );
    let branch = git_output(&created.worktree_path, &["symbolic-ref", "--short", "HEAD"])?;
    assert_eq!(
        branch, name.branch_name,
        "the thread's branch is checked out"
    );
    Ok(created.worktree_path)
}

fn check_thread_worktree(worktree: &Path) -> Outcome {
    assert!(worktree.join(".env").is_file(), "configured files copied");
    assert!(
        worktree.join("setup-ran.txt").is_file(),
        "setup ran at the final path"
    );
    let build = Command::new("cargo")
        .current_dir(worktree)
        .args(["build", "--workspace", "-v"])
        .envs(velocity_git::compiler_cache_env())
        .env_remove("CARGO_TARGET_DIR")
        .output()?;
    let log = String::from_utf8_lossy(&build.stderr);
    assert!(build.status.success(), "{log}");
    assert!(
        log.contains("Fresh pooltest") && !log.contains("Compiling pooltest"),
        "the prebuild survived the move: {log}"
    );
    println!("first build in the thread worktree: Fresh (prebuilt)");
    Ok(())
}

/// A thread's worktree can never be removed through the pool, and pool slots never hold a
/// branch (F5 Guards).
fn check_guards(repository: &Path, worktree: &Path) -> Outcome {
    assert!(velocity_git::remove_ready_worktree(repository, worktree).is_err());
    assert!(worktree.is_dir(), "the thread's worktree is untouched");
    let listed = git_output(repository, &["worktree", "list", "--porcelain"])?;
    let slots_with_branch = listed
        .split("\n\n")
        .filter(|entry| entry.contains(".ready-") && entry.contains("branch "))
        .count();
    assert_eq!(slots_with_branch, 0, "pool slots are detached");
    Ok(())
}

fn wait_for(pools: &ReadyPools, what: &str, done: impl Fn(&[SlotState]) -> bool) -> Outcome {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let states: Vec<SlotState> = pools.rows(1).into_iter().map(|row| row.state).collect();
        if done(&states) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {what}: {states:?}").into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn scratch_store(root: &Path, repository: &Path) -> Outcome<StateStore> {
    let state = root.join("state");
    std::fs::create_dir_all(&state)?;
    let mut persisted = PersistedState::default();
    let worktrees = &mut persisted.settings.worktrees;
    worktrees.location_root = Some(root.join("wt").display().to_string());
    worktrees.share_rust_build_cache = true;
    worktrees.copy_files = ".env".into();
    worktrees.setup_command = "echo ran> setup-ran.txt".into();
    worktrees.warm_pool_count = 2;
    let mut project = ProjectItem::new(1, "pooltest".into(), repository.display().to_string());
    project.default_branch = "master".into();
    project.spec_locked = true;
    persisted.projects.push(project);
    std::fs::write(
        state.join("state.json"),
        serde_json::to_string_pretty(&persisted)?,
    )?;
    Ok(StateStore::load_or_default(&state)?)
}

/// A one-crate Cargo project with a bare `origin`, like a cloned GitHub project.
fn scratch_repository(root: &Path) -> Outcome<PathBuf> {
    let origin = root.join("origin.git");
    let repository = root.join("repo");
    std::fs::create_dir_all(&origin)?;
    std::fs::create_dir_all(repository.join("src"))?;
    git(&origin, &["init", "--bare", "-b", "master"])?;
    git(&repository, &["init", "-b", "master"])?;
    git(&repository, &["config", "user.name", "Velocity Example"])?;
    git(
        &repository,
        &["config", "user.email", "velocity@example.invalid"],
    )?;
    std::fs::write(
        repository.join("Cargo.toml"),
        "[package]\nname = \"pooltest\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )?;
    std::fs::write(
        repository.join("src/lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )?;
    std::fs::write(
        repository.join(".gitignore"),
        "target/\n.env\nsetup-ran.txt\n",
    )?;
    std::fs::write(repository.join(".env"), "TOKEN_NAME=example\n")?;
    git(&repository, &["add", "."])?;
    git(&repository, &["commit", "-m", "Start"])?;
    git(
        &repository,
        &["remote", "add", "origin", &origin.display().to_string()],
    )?;
    git(&repository, &["push", "-q", "origin", "master"])?;
    Ok(repository)
}

/// A merge lands on GitHub: `origin/master` moves while the local clone stays behind.
fn advance_master(root: &Path, repository: &Path) -> Outcome<String> {
    let other = root.join("other");
    git(
        root,
        &[
            "clone",
            "-q",
            &root.join("origin.git").display().to_string(),
            "other",
        ],
    )?;
    git(&other, &["config", "user.name", "Velocity Example"])?;
    git(
        &other,
        &["config", "user.email", "velocity@example.invalid"],
    )?;
    std::fs::write(other.join("src/lib.rs"), "pub fn answer() -> u32 { 43 }\n")?;
    git(&other, &["commit", "-qam", "Merge #2"])?;
    git(&other, &["push", "-q", "origin", "master"])?;
    let tip = git_output(&other, &["rev-parse", "HEAD"])?;
    assert_ne!(git_output(repository, &["rev-parse", "master"])?, tip);
    Ok(tip)
}

fn git(directory: &Path, arguments: &[&str]) -> Outcome {
    let status = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .status()?;
    if !status.success() {
        return Err(format!("git {arguments:?} failed in {}", directory.display()).into());
    }
    Ok(())
}

fn git_output(directory: &Path, arguments: &[&str]) -> Outcome<String> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
