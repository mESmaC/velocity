use velocity_store::{PersistedState, ProjectItem, StateStore, ThreadItem};

// A state directory spelled with `/`, `.` or `..` must load and save the same
// state.json as its plain spelling (#215); a wrong spelling loses or forks saved state.
#[test]
fn state_saved_through_a_mixed_spelling_lands_in_the_plain_state_file()
-> Result<(), Box<dyn std::error::Error>> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!("velocity-215-{}-{unique}", std::process::id()));
    let plain = root.join("restart smoke 215");
    std::fs::create_dir_all(&plain)?;
    let mut state = PersistedState::default();
    let mut project = ProjectItem::new(1, "saved".into(), plain.to_string_lossy().into());
    project
        .threads
        .push(ThreadItem::new(1, "kept until removed".into()));
    state.projects.push(project);
    let state_file = plain.join("state.json");
    std::fs::write(&state_file, serde_json::to_vec(&state)?)?;

    let mixed = root.join("./unused/../restart smoke 215");
    let mut store = StateStore::load_or_default(&mixed)?;
    store.remove_thread(1, 1)?;
    drop(store);

    let saved: PersistedState = serde_json::from_slice(&std::fs::read(&state_file)?)?;
    assert!(saved.projects[0].threads.is_empty());
    assert!(!root.join("unused").exists());
    let reloaded = StateStore::load_or_default(&plain)?;
    assert!(
        reloaded
            .project(1)
            .ok_or("missing project")?
            .threads
            .is_empty()
    );
    drop(reloaded);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
