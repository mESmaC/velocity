use serde_json::json;
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_store::{ProjectItem, Settings};
use velocity_threads::plan_launch_with_preferences as plan_launch;

fn project(repository: &str) -> Result<ProjectItem, serde_json::Error> {
    let threads = (1..=999).map(|identifier| {
        let branch = if identifier == 1 { "vel/repeat".to_owned() }
            else { format!("vel/repeat-{identifier}") };
        json!({
            "id":identifier, "title":"Completed job", "state":"merged",
            "activity":"idle", "branch_name":branch, "worktree_path":null,
            "pull_request_number":identifier, "review_stage":null, "task_ids":[],
            "additions":0, "deletions":0, "elapsed_seconds":0,
            "provider":"claude", "model_name":"agent default", "effort":"low",
            "preferred_speed":"regular", "effective_speed":"regular",
            "model_supports_fast":false
        })
    }).collect::<Vec<_>>();
    serde_json::from_value(json!({
        "id":1, "name":"review-name-history", "location_kind":"local",
        "repository_path":repository, "default_branch":"master", "github_remote":null,
        "spec_path":"SPEC.md", "spec_locked":true, "active_thread_id":null,
        "merged_today":0, "tasks":[], "threads":threads, "pending_interview":null
    }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repository = std::env::current_dir()?;
    let project = project(&repository.to_string_lossy())?;
    let preferences = WorktreePreferences {
        root:repository.join(".tools/history worktrees").display().to_string(),
        project_override:None, delete_local_branch:true, share_build_cache:false,
        copy_files:String::new(), setup_command:String::new(), ready_count:0,
    };
    match plan_launch(&project, &preferences, &Settings::default(), "repeat") {
        Err(error) => println!("PASS: exhausted names refused before effects: {error}"),
        Ok(plan) => {
            let reused = project.threads.iter().any(|thread| thread.branch_name == plan.thread.branch_name);
            println!("planned branch: {}", plan.thread.branch_name);
            println!("worktree already exists: {}", plan.worktree.worktree_path.exists());
            println!("branch already belongs to saved history: {reused}");
            if reused {
                println!("FAIL: a new job reused a completed job's branch identity");
                std::process::exit(1);
            }
            println!("PASS: a new branch identity was selected");
        }
    }
    Ok(())
}
