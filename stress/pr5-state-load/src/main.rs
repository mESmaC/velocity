//! PR duty stress for #5: persisted-state load on bad and edge-case input.
use std::fs;
use velocity_store::{PersistedState, StateStore, ThreadState};

fn fresh_directory(name: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!("velocity pr5 stress {name}"));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    directory
}

fn main() {
    let mut failures = 0;

    // 1. A truncated or corrupt state.json (power loss, disk error, hand edit).
    let directory = fresh_directory("corrupt");
    fs::write(directory.join("state.json"), b"{\"revision\": 3, \"projects\": [").unwrap();
    match StateStore::load_or_default(&directory) {
        Ok(_) => println!("corrupt state.json: loaded (recovered)"),
        Err(error) => {
            failures += 1;
            println!("FAIL corrupt state.json: load_or_default -> Err: {error}");
            println!("     src-tauri setup calls state::initialize(..)? so the app does not start.");
        }
    }

    // 2. Threads that must have no worktree (spec interview: F3/F5; merged: worktree deleted).
    let directory = fresh_directory("no-worktree");
    let mut state = PersistedState::default();
    let mut project: serde_json::Value = serde_json::json!({
        "id": 1, "name": "demo", "location_kind": "local", "repository_path": "D:/demo",
        "default_branch": "master", "github_remote": null, "spec_path": "SPEC.md",
        "spec_locked": true, "active_thread_id": null, "merged_today": 0, "tasks": [], "threads": []
    });
    let state_names = ["spec", "merged"];
    for (index, thread_state) in state_names.iter().enumerate() {
        project["threads"].as_array_mut().unwrap().push(serde_json::json!({
            "id": index, "title": "t", "state": thread_state, "activity": "idle",
            "branch_name": "", "worktree_path": "", "worktree_path_id": null,
            "pull_request_number": null, "review_stage": null, "task_ids": [],
            "additions": 0, "deletions": 0, "elapsed_seconds": 0, "provider": "claude",
            "model_name": "m", "model_id": 1, "effort": "low", "preferred_speed": "regular",
            "effective_speed": "regular", "model_supports_fast": false
        }));
    }
    state.projects.push(serde_json::from_value(project).unwrap());
    fs::write(directory.join("state.json"), serde_json::to_vec(&state).unwrap()).unwrap();
    let store = StateStore::load_or_default(&directory).unwrap();
    for thread in &store.state().projects[0].threads {
        let label = match thread.state { ThreadState::Spec => "spec", ThreadState::Merged => "merged", _ => "other" };
        if thread.worktree_path_id.is_some() {
            failures += 1;
            println!("FAIL {label} thread: worktree_path_id None -> {:?} after load", thread.worktree_path_id);
        } else {
            println!("{label} thread: still has no worktree");
        }
    }

    println!("failures: {failures}");
    std::process::exit(if failures == 0 { 0 } else { 1 });
}