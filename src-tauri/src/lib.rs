use anyhow::Context;
use std::path::Path;
use tauri_specta::{Builder, Event, collect_commands, collect_events};
use velocity_store::PersistedState;

mod agent_activity;
mod agents;
pub mod airc_access;
pub mod airc_claims;
mod airc_delivery;
mod airc_events;
mod airc_modal;
mod build_gate;
pub mod duty;
mod follow_through;
mod github;
mod goal_swarm;
mod interview;
mod issue_panel;
pub mod layout;
pub mod main_watch;
mod new_project;
mod parking;
mod projects;
mod proof_card;
mod proofs_settings;
mod provider_agents;
mod providers;
mod pull_request_panel;
mod ready_pool;
pub mod reviews;
mod shared_mcp;
mod skills;
mod state;
mod swarm;
mod swarm_rebase;
pub mod sweeps;
mod tasks;
mod thread_menu;
mod threads;
mod window;
mod work_pool;
mod worktree_settings;

pub use layout::{apply_agent_event, record_agent_event};

fn bindings() -> Builder<tauri::Wry> {
    // Export event payloads/names without the generator's `any`-typed helpers;
    // ipc.ts supplies typed listeners, while run() registers native emitters.
    let builder = Builder::new().commands(collect_commands![
        agents::read_agents_md,
        agents::reload_agents_md,
        agents::edit_agents_md,
        agents::write_agents_md,
        agents::tighten_rule,
        airc_delivery::airc_send,
        airc_events::airc_events,
        airc_events::airc_messages,
        airc_events::airc_ledger,
        airc_modal::get_airc_modal,
        airc_modal::set_airc_message_mode,
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
        proofs_settings::get_proofs_settings,
        provider_agents::set_provider_command,
        provider_agents::add_custom_agent,
        provider_agents::remove_custom_agent,
        providers::list_providers,
        providers::provider_sign_in,
        providers::provider_sign_out,
        providers::set_provider_settings,
        providers::get_model_menu,
        providers::choose_model,
        work_pool::get_work_pool,
        worktree_settings::list_worktrees,
        worktree_settings::set_worktree_settings,
        worktree_settings::preview_worktree_folder,
        worktree_settings::browse_worktree_folder,
        worktree_settings::prune_worktrees,
        duty::get_duty_dialog_defaults,
        duty::start_duty,
        duty::pause_duty,
        duty::add_duty_agents,
        github::list_pull_requests,
        github::get_pull_request,
        github::dispatch_pull_request,
        goal_swarm::get_goal_swarm_dialog,
        goal_swarm::read_goal_swarm_pool,
        goal_swarm::launch_goal_swarm,
        goal_swarm::add_goal_agents,
        goal_swarm::get_goal_swarm_board,
        issue_panel::get_issue_panel,
        issue_panel::open_project_issue,
        issue_panel::dispatch_issue,
        pull_request_panel::get_pull_request_panel,
        pull_request_panel::open_project_pull_request,
        shared_mcp::edit_shared_mcp_servers,
        skills::get_skill_library,
        skills::get_skill_settings,
        skills::set_skill_settings,
        skills::install_skill_pack,
        skills::check_skill_pack_update,
        skills::confirm_skill_pack_update,
        skills::generate_design_system,
        swarm::get_swarm_dialog,
        swarm::launch_swarm,
        swarm::add_to_swarm,
        tasks::add_task,
        tasks::list_tasks,
        thread_menu::get_thread_menu,
        thread_menu::run_thread_menu_action,
        thread_menu::delete_thread,
        thread_menu::undo_delete_thread,
        new_project::get_new_project_defaults,
        new_project::pick_project_folder,
        new_project::create_project,
        new_project::open_project,
        new_project::get_github_repositories,
        build_gate::get_launch_eligibility,
    ]);
    event_bindings(builder)
}

fn event_bindings(builder: Builder<tauri::Wry>) -> Builder<tauri::Wry> {
    builder
        .typ::<PersistedState>()
        .typ::<airc_events::AircEventPublished>()
        .typ::<airc_events::AircMessagePublished>()
        .typ::<airc_modal::AircModalPublished>()
        .typ::<duty::DutyEventPublished>()
        .typ::<follow_through::PullRequestLifecycleEvent>()
        .typ::<window::WindowStateChanged>()
        .typ::<layout::LayoutStateChanged>()
        .typ::<layout::LayoutIntentRequested>()
        .typ::<projects::ProjectSelected>()
        .typ::<providers::ProvidersChanged>()
        .typ::<swarm::SwarmEventPublished>()
        .typ::<issue_panel::IssuePanelChanged>()
        .typ::<worktree_settings::WorktreeInventoryChanged>()
        .typ::<pull_request_panel::PullRequestPanelChanged>()
        .typ::<threads::ThreadStatusChanged>()
        .typ::<threads::ThreadStepRecorded>()
        .constant("AIRC_EVENT", airc_events::AircEventPublished::NAME)
        .constant(
            "AIRC_MESSAGE_EVENT",
            airc_events::AircMessagePublished::NAME,
        )
        .constant("AIRC_MODAL_EVENT", airc_modal::AircModalPublished::NAME)
        .constant("DUTY_EVENT", duty::DutyEventPublished::NAME)
        .constant("PROJECT_SELECTED_EVENT", projects::ProjectSelected::NAME)
        .constant("PROVIDERS_EVENT", providers::ProvidersChanged::NAME)
        .constant("SWARM_EVENT", swarm::SwarmEventPublished::NAME)
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
        airc_events::AircEventPublished,
        airc_events::AircMessagePublished,
        airc_modal::AircModalPublished,
        duty::DutyEventPublished,
        window::WindowStateChanged,
        layout::LayoutStateChanged,
        layout::LayoutIntentRequested,
        projects::ProjectSelected,
        providers::ProvidersChanged,
        swarm::SwarmEventPublished,
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
        .manage(goal_swarm::GoalSwarmPool::default())
        .manage(layout::MainLayout::default())
        .manage(new_project::ManagedRepositoryPicker::default())
        .manage(providers::ManagedProviders::default())
        .manage(airc_modal::ManagedAircModal::default())
        .manage(threads::ManagedThreads::default())
        .manage(velocity_interview::Interviews::default())
        .manage(thread_menu::ManagedDeletions::default())
        .invoke_handler(bindings.invoke_handler())
        .setup(move |application| {
            bindings.mount_events(application);
            state::initialize(application.handle())?;
            airc_access::initialize(application.handle())?;
            projects::initialize(application.handle())?;
            providers::initialize(application.handle());
            follow_through::initialize(application.handle(), threads::follow_through_hooks());
            issue_panel::initialize(application.handle());
            worktree_settings::initialize(application.handle())?;
            ready_pool::initialize(application.handle())?;
            pull_request_panel::initialize(application.handle());
            skills::initialize(application.handle())?;
            interview::start(application.handle());
            reviews::initialize(application.handle())?;
            sweeps::initialize(application.handle())?;
            main_watch::initialize(application.handle())?;
            airc_delivery::initialize(application.handle());
            airc_modal::initialize(application.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            window::on_window_event(window, event);
            worktree_settings::on_window_event(window, event);
        })
        .run(tauri::generate_context!())
        .context("Could not run Velocity")
}
