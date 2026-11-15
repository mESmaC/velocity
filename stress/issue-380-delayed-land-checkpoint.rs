//! Risk (#380 review): a delayed land reply must not erase a newer revert's saved
//! fixing state and durable reopen message. The event follower and checker run independently.
use std::error::Error;
use velocity_store::rosen::{RosenFollowUp, RosenProgress};
use velocity_store::{PersistedState, ProjectItem, ReviewStage, StateStore, ThreadItem, ThreadState};

fn main() -> Result<(), Box<dyn Error>> {
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    let directory = std::env::temp_dir().join(format!("velocity-380-delayed-land-{unique}"));
    std::fs::create_dir_all(&directory)?;
    let mut state = PersistedState::default();
    let mut project = ProjectItem::new(1, "Review".into(), r"C:\project one".into());
    let mut thread = ThreadItem::new(4, "Build fix".into());
    thread.rosen_change = Some("culprit".into());
    thread.state = ThreadState::Review;
    thread.review_stage = Some(ReviewStage::Review);
    project.threads.push(thread);
    state.projects.push(project);
    std::fs::write(directory.join("state.json"), serde_json::to_vec(&state)?)?;
    let mut store = StateStore::load_or_default(&directory)?;

    // The follower receives land then a confirmed build revert while the checker
    // awaits its land response or the store lock.
    store.save_rosen_progress(1, 10, "culprit", (RosenProgress::Landed, ""))?;
    store.save_rosen_progress(1, 11, "culprit", (RosenProgress::Reverted, "cargo build failed"))?;
    println!("Before delayed callback: {:?}, {:?}",
        store.state().projects[0].threads[0].review_stage, store.rosen_follow_ups(1));

    // Exact save made by src-tauri/src/rosen_events.rs::record_landed.
    store.save_rosen_progress(1, 0, "culprit", (RosenProgress::Landed, ""))?;
    let thread = &store.state().projects[0].threads[0];
    let pending = store.rosen_follow_ups(1);
    println!("After delayed callback: {:?}, {:?}, cursor {}",
        thread.review_stage, pending, store.rosen_event_cursor(1));
    assert_eq!(thread.state, ThreadState::Review, "delayed land reply overwrote a confirmed revert");
    assert_eq!(thread.review_stage, Some(ReviewStage::Fixing));
    assert!(pending.iter().any(|(_, _, steps)| steps.iter().any(|step|
        matches!(step, RosenFollowUp::Reopen { .. }))), "revert recovery was discarded");
    Ok(())
}
