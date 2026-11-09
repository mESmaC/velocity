use anyhow::Context;
use std::path::Path;
use tauri_specta::{Builder, Event, collect_commands, collect_events};
use velocity_store::PersistedState;

mod agents;
mod build_gate;
mod follow_through;
mod github;
mod interview;
mod issue_panel;
pub mod layout;
mod new_project;
mod projects;
mod pull_request_panel;
mod state;
mod thread_menu;
mod threads;
mod window;
mod worktree_settings;

pub use layout::{apply_agent_event, record_agent_event};

fn bindings() -> Builder<tauri::Wry> {
    // Export event payloads/names without the generator's `any`-typed helpers;
    // ipc.ts supplies typed listeners, while run() registers native emitters.
    let builder = Builder::new().commands(collect_commands![
        agents::read_agents_md,
        agents::edit_agents_md,
        agents::write_agents_md,
        window::get_window_state,
        window::minimize_window,
        window::toggle_maximize_window,
        window::close_window,
        state::get_settings,
        state::set_settings,
        layout::get_layout_state,
        layout::dispatch_layout,
        projects::list_projects,
        projects::switch_project,
        worktree_settings::list_worktrees,
        worktree_settings::set_worktree_settings,
        worktree_settings::preview_worktree_folder,
        worktree_settings::browse_worktree_folder,
        worktree_settings::prune_worktrees,
        github::list_pull_requests,
        github::get_pull_request,
        github::dispatch_pull_request,
        issue_panel::get_issue_panel,
        issue_panel::open_project_issue,
        issue_panel::dispatch_issue,
        pull_request_panel::get_pull_request_panel,
        pull_request_panel::open_project_pull_request,
        thread_menu::get_thread_menu,
        thread_menu::run_thread_menu_action,
        thread_menu::delete_thread,
        thread_menu::undo_delete_thread,
        new_project::get_new_project_defaults,
        new_project::pick_project_folder,
        new_project::create_project,
        build_gate::get_launch_eligibility,
    ]);
    event_bindings(builder)
}

fn event_bindings(builder: Builder<tauri::Wry>) -> Builder<tauri::Wry> {
    builder
        .typ::<PersistedState>()
        .typ::<follow_through::PullRequestLifecycleEvent>()
        .typ::<window::WindowStateChanged>()
        .typ::<layout::LayoutStateChanged>()
        .typ::<layout::LayoutIntentRequested>()
        .typ::<projects::ProjectSelected>()
        .typ::<issue_panel::IssuePanelChanged>()
        .typ::<worktree_settings::WorktreeInventoryChanged>()
        .typ::<pull_request_panel::PullRequestPanelChanged>()
        .typ::<threads::ThreadStatusChanged>()
        .typ::<threads::ThreadStepRecorded>()
        .constant("PROJECT_SELECTED_EVENT", projects::ProjectSelected::NAME)
        .constant("ISSUE_PANEL_EVENT", issue_panel::IssuePanelChanged::NAME)
        .constant("WINDOW_STATE_EVENT", window::WindowStateChanged::NAME)
        .constant("LAYOUT_STATE_EVENT", layout::LayoutStateChanged::NAME)
        .constant("LAYOUT_INTENT_EVENT", layout::LayoutIntentRequested::NAME)
        .constant("PR_OPENED_EVENT", follow_through::PR_OPENED_EVENT)
        .constant("PR_CI_EVENT", follow_through::PR_CI_EVENT)
        .constant("PR_REVIEW_EVENT", follow_through::PR_REVIEW_EVENT)
        .constant("PR_MERGED_EVENT", follow_through::PR_MERGED_EVENT)
        .constant("PR_CLOSED_EVENT", follow_through::PR_CLOSED_EVENT)
        .constant(
            "WORKTREE_INVENTORY_EVENT",
            worktree_settings::WorktreeInventoryChanged::NAME,
        )
        .constant(
            "PULL_REQUEST_PANEL_EVENT",
            pull_request_panel::PullRequestPanelChanged::NAME,
        )
        .constant("THREAD_STATUS_EVENT", threads::ThreadStatusChanged::NAME)
        .constant("THREAD_STEP_EVENT", threads::ThreadStepRecorded::NAME)
        .error_handling(tauri_specta::ErrorHandlingMode::Throw)
}

/// Use a manifest-relative path so export also works from a checkout with spaces.
pub fn export_bindings() -> anyhow::Result<()> {
    let destination = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/src/lib/bindings.ts");
    bindings()
        .export(specta_typescript::Typescript::default(), &destination)
        .context("Could not generate the frontend command and event bindings")?;
    // Keep generated output deterministic without a trailing blank line.
    let generated = std::fs::read_to_string(&destination)?;
    std::fs::write(destination, format!("{}\n", generated.trim_end()))
        .context("Could not normalize the generated bindings")
}

pub fn run() -> anyhow::Result<()> {
    #[cfg(debug_assertions)]
    export_bindings()?;

    let bindings = bindings().events(collect_events![
        window::WindowStateChanged,
        layout::LayoutStateChanged,
        layout::LayoutIntentRequested,
        projects::ProjectSelected,
        issue_panel::IssuePanelChanged,
        worktree_settings::WorktreeInventoryChanged,
        pull_request_panel::PullRequestPanelChanged,
        threads::ThreadStatusChanged,
        threads::ThreadStepRecorded
    ]);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(agents::ManagedAgents::default())
        .manage(layout::MainLayout::default())
        .manage(threads::ManagedThreads::default())
        .manage(velocity_interview::Interviews::default())
        .manage(thread_menu::ManagedDeletions::default())
        .invoke_handler(bindings.invoke_handler())
        .setup(move |application| {
            bindings.mount_events(application);
            state::initialize(application.handle())?;
            projects::initialize(application.handle())?;
            follow_through::initialize(application.handle(), threads::follow_through_hooks());
            issue_panel::initialize(application.handle());
            worktree_settings::initialize(application.handle())?;
            pull_request_panel::initialize(application.handle());
            interview::start(application.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            window::on_window_event(window, event);
            worktree_settings::on_window_event(window, event);
        })
        .run(tauri::generate_context!())
        .context("Could not run Velocity")
}
