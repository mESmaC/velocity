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

// Persisted thread agent names must survive save/load, while pre-#532 saved threads without
// `agent_name` must still deserialize and keep their legacy AIRC/Rosen identity (#532).
#[test]
fn thread_agent_name_survives_save_load_and_keeps_legacy_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "velocity-agent-name-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir(&directory)?;
    let mut state = PersistedState::default();
    let mut project = ProjectItem::new(1, "agent names".into(), directory.to_string_lossy().into());
    let mut legacy = ThreadItem::new(49, "I'd like your honest".into());
    legacy.worktree_path = Some("Z:\\work\\i-d-like-your-honest".into());
    legacy.rosen_change = Some("zrqrpzzruukpynwnvyporzrwpxplrvqq".into());
    project.threads.push(legacy);
    state.projects.push(project);
    let path = directory.join("state.json");
    std::fs::write(&path, serde_json::to_vec(&state)?)?;

    let mut store = StateStore::load_or_default(&directory)?;
    let settings = store.settings();
    let build_id = store.add_thread(
        1,
        velocity_store::threads::NewThread::new(
            "I'd like your honest feedback".into(),
            "vel/i-d-like-your-honest-2".into(),
            "Z:\\work\\i-d-like-your-honest-2".into(),
            &settings,
        ),
    )?;
    assert_eq!(build_id, 50);
    drop(store);

    let loaded = StateStore::load_or_default(&directory)?;
    let project = loaded.project(1).ok_or("missing saved project")?;
    let loaded_build = project
        .threads
        .iter()
        .find(|thread| thread.id == build_id)
        .ok_or("missing build thread")?;
    assert_eq!(loaded_build.agent_name(), "i-d-like-your-honest-2-50");
    assert_eq!(loaded_build.airc_member(), "i-d-like-your-honest-2-50");
    assert_eq!(loaded_build.rosen_agent_name(), "i-d-like-your-honest-2-50");
    assert!(loaded_build.matches_airc_member("i-d-like-your-honest-2-50"));

    let loaded_legacy = project
        .threads
        .iter()
        .find(|thread| thread.id == 49)
        .ok_or("missing legacy thread")?;
    assert!(loaded_legacy.agent_name.is_none());
    assert_eq!(loaded_legacy.airc_member(), "thread-49");
    assert_eq!(loaded_legacy.rosen_agent_name(), "i-d-like-your-honest");
    assert!(loaded_legacy.matches_airc_member("thread-49"));

    drop(loaded);
    std::fs::remove_file(path)?;
    std::fs::remove_dir(directory)?;
    Ok(())
}
