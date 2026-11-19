use std::path::PathBuf;
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutIntent};
use velocity_core::navigation::{NavigationIntent, NavigationView};
use velocity_store::threads::NewThread;
use velocity_store::{EffortLevel, NewProjectRecord, ProjectLocationKind, ProviderKind, SpeedMode, StateStore};

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    let checkout = root.join("project");
    std::fs::create_dir_all(&checkout).expect("project folder");
    let mut store = StateStore::load_or_default(root.join("state")).expect("fresh store");
    let project = store.commit_new_project(NewProjectRecord {
        name: "project".into(), location_kind: ProjectLocationKind::Local,
        repository_path: checkout.to_string_lossy().into_owned(), default_branch: "master".into(),
        github_remote: None }, None, true).expect("project");
    let thread = store.add_thread(project, NewThread {
        title: "build thread".into(), branch_name: "vel/build-thread".into(),
        worktree_path: root.join("worktree").to_string_lossy().into_owned(),
        provider: ProviderKind::Claude, model_name: "agent default".into(), model_supports_fast: false,
        effort: EffortLevel::High, preferred_speed: SpeedMode::Regular }).expect("thread");
    let mut layout = LayoutState::default();
    store.refresh_layout(&mut layout);
    layout.select_thread(usize::try_from(thread).expect("identifier"));
    let mut failures = report("thread-open", layout.snapshot().view == NavigationView::Thread,
        format!("view={:?}", layout.snapshot().view));
    layout.apply(LayoutAction::Request(LayoutIntent::Navigation(NavigationIntent::Settings)));
    failures += report("settings-open", layout.snapshot().view == NavigationView::Settings,
        format!("view={:?}", layout.snapshot().view));
    // What every saved-state change does next: an agent status update, for example.
    store.refresh_layout(&mut layout);
    failures += report("settings-survives-refresh", layout.snapshot().view == NavigationView::Settings,
        format!("expected=Settings, actual={:?}", layout.snapshot().view));
    println!("failures: {failures}");
    std::process::exit(i32::from(failures != 0));
}
