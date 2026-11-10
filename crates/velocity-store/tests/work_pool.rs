use velocity_store::{PersistedState, ProjectItem, StateStore, ThreadItem, ThreadState};

// Losing this saved identity would offer completed work again after history deletion.
#[test]
fn completed_spec_work_survives_history_deletion_and_reload()
-> Result<(), Box<dyn std::error::Error>> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("velocity-pool-{}-{unique}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let mut state = PersistedState::default();
    let mut project = ProjectItem::new(1, "saved pool".into(), directory.to_string_lossy().into());
    let mut merged = ThreadItem::new(1, "completed".into());
    merged.state = ThreadState::Merged;
    merged.spec_section = Some("Features / Durable work".into());
    project.threads.push(merged);
    state.projects.push(project);
    let path = directory.join("state.json");
    std::fs::write(&path, serde_json::to_vec(&state)?)?;
    let mut store = StateStore::load_or_default(&directory)?;
    store.remove_thread(1, 1)?;
    drop(store);
    let loaded = StateStore::load_or_default(&directory)?;
    let project = loaded.project(1).ok_or("missing saved project")?;
    assert!(project.threads.is_empty());
    assert_eq!(project.built_spec_sections, ["Features / Durable work"]);
    drop(loaded);
    std::fs::remove_file(path)?;
    std::fs::remove_dir(directory)?;
    Ok(())
}
