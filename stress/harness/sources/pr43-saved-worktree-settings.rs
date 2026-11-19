use std::{fs, path::PathBuf, process::{Command, exit}};
use velocity_store::{NewProjectRecord, ProjectLocationKind, StateStore};
use velocity_threads::plan_launch;

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    let repository = root.join("project with spaces");
    fs::create_dir_all(&repository).expect("fixture directory");
    assert!(Command::new("git").args(["init", "--initial-branch", "master"])
        .arg(&repository).output().expect("git init").status.success());
    let state_path = root.join("state");
    let mut store = StateStore::load_or_default(&state_path).expect("fresh store");
    let identifier = store.commit_new_project(NewProjectRecord {
        name: "project".into(), location_kind: ProjectLocationKind::Local,
        repository_path: repository.to_string_lossy().into_owned(),
        default_branch: "master".into(), github_remote: None,
    }, None, true).expect("register project");
    let project_root = root.join("saved project override");
    let mut settings = store.settings();
    settings.worktrees.location_root = Some(root.join("saved global root").to_string_lossy().into_owned());
    settings.worktrees.project_roots.insert(identifier, project_root.to_string_lossy().into_owned());
    settings.worktrees.copy_files = "saved.config, config with spaces.json".into();
    settings.worktrees.setup_command = "echo saved project setup".into();
    settings.worktrees.share_rust_build_cache = false;
    store.set_settings(settings).expect("save settings");
    let store = StateStore::load_or_default(&state_path).expect("reload saved settings");
    let settings = store.settings();
    let plan = plan_launch(store.active_project().expect("active project"), &settings, "check settings")
        .expect("plan without starting an agent");
    let mut failures = report("saved-settings-survive-reload", settings.worktrees.project_roots.contains_key(&identifier)
        && settings.worktrees.setup_command == "echo saved project setup", "read actual persisted settings");
    failures += report("launch-uses-project-override", plan.worktree.worktree_path.parent() == Some(project_root.as_path()),
        format!("expected parent={}, actual={}", project_root.display(), plan.worktree.worktree_path.display()));
    failures += report("launch-uses-saved-copy-files", plan.worktree.untracked_files == settings.worktrees.copy_files,
        format!("expected={:?}, actual={:?}", settings.worktrees.copy_files, plan.worktree.untracked_files));
    failures += report("launch-uses-saved-setup-command", plan.worktree.setup_command == settings.worktrees.setup_command,
        format!("expected={:?}, actual={:?}", settings.worktrees.setup_command, plan.worktree.setup_command));
    failures += report("shared-cache-toggle-control", !plan.worktree.settings.share_build_cache
        && !plan.agent.env.iter().any(|(key, _)| key == "CARGO_TARGET_DIR"), "existing cache toggle is honored");
    println!("failures: {failures}");
    exit(i32::from(failures != 0));
}
