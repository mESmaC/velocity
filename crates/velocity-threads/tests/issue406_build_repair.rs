use std::error::Error;
use std::path::Path;

use velocity_rosen::build_repairs::BuildFailure;
use velocity_store::{PersistedState, ProjectItem, StateStore};
use velocity_threads::build_repairs::prepare_repair;

// Risk (#406): refused inverses left the base broken; restarting or replaying a refusal
// must retain one repair and its evidence while later culprits receive independent threads.
#[test]
fn repair_ownership_and_pending_launch_survive_restart() -> Result<(), Box<dyn Error>> {
    let temporary_root = std::env::temp_dir().canonicalize()?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory = temporary_root.join(format!("velocity-406-{}-{unique}", std::process::id()));
    seed_old_snapshot(&directory)?;
    let mut store = StateStore::load_or_default(&directory)?;
    let failure = BuildFailure::new(
        "1".repeat(40),
        "culprit-change".into(),
        "cargo build --locked failed: stale Rosen pin".into(),
        1_000,
    )
    .with_refusal("Inverse overlaps later work".into(), vec!["2".repeat(40)]);
    assert!(failure.needs_repair(1_000));
    let (plan, identifier) = prepare_repair(&mut store, 1, &failure)?.ok_or("missing repair")?;
    assert_eq!(plan.thread.model_name, "implementation-model");
    assert!(plan.title.request.is_none());
    assert!(plan.prompt.contains(&failure.output));
    assert!(plan.prompt.contains(&failure.commit));
    assert!(plan.prompt.contains(&failure.blocking_commits[0]));
    assert!(
        plan.prompt
            .contains("Make the project's base build again and touch nothing else.")
    );
    drop(store);

    let mut store = StateStore::load_or_default(&directory)?;
    store.reset_disconnected_threads()?;
    let (recovered, same) = prepare_repair(&mut store, 1, &failure)?.ok_or("lost launch")?;
    assert_eq!(same, identifier);
    assert_eq!(recovered.prompt, plan.prompt);
    assert_eq!(recovered.thread.worktree_path, plan.thread.worktree_path);
    store.update_thread(1, same, |thread| thread.pending_rosen_launch = None)?;
    assert!(prepare_repair(&mut store, 1, &failure)?.is_none());
    independent_culprit(&mut store, identifier)?;
    store.close_build_repairs(1, std::slice::from_ref(&failure.commit))?;
    assert!(store.project(1).ok_or("missing project")?.build_repairs[0].closed);
    store.remove_thread(1, identifier)?;
    drop(store);

    let mut store = StateStore::load_or_default(&directory)?;
    assert!(prepare_repair(&mut store, 1, &failure)?.is_none());
    assert_eq!(store.project(1).ok_or("missing project")?.threads.len(), 1);
    assert_eq!(directory.parent(), Some(temporary_root.as_path()));
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

fn independent_culprit(store: &mut StateStore, first: u32) -> Result<(), Box<dyn Error>> {
    let mut later = BuildFailure::new(
        "3".repeat(40),
        "later-change".into(),
        "clippy failed in a later land".into(),
        1_000,
    );
    assert!(!later.needs_repair(120_999));
    assert!(later.needs_repair(121_000));
    let (_, second) = prepare_repair(store, 1, &later)?.ok_or("later culprit was hidden")?;
    assert_ne!(first, second);
    assert_eq!(store.project(1).ok_or("missing project")?.threads.len(), 2);
    later.reverted = true;
    assert!(!later.needs_repair(121_000));
    Ok(())
}

fn seed_old_snapshot(directory: &Path) -> Result<(), Box<dyn Error>> {
    std::fs::create_dir_all(directory)?;
    let mut project = ProjectItem::new(1, "Repair project".into(), directory.display().to_string());
    project.spec_locked = true;
    let mut state = PersistedState::default();
    state.projects.push(project);
    state.settings.providers.default_model = Some("implementation-model".into());
    let mut snapshot = serde_json::to_value(state)?;
    snapshot["projects"][0]
        .as_object_mut()
        .ok_or("missing project object")?
        .remove("build_repairs");
    std::fs::write(directory.join("state.json"), serde_json::to_vec(&snapshot)?)?;
    Ok(())
}
