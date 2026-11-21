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
mod base_ci;
mod browser;
mod build_gate;
mod code_graph;
mod collapse;
mod collapse_connection;
mod collapse_mirror;
mod continuous_dispatch;
mod diagnostics;
pub mod duty;
mod github_requests;
mod goal_stack;
mod goal_swarm;
pub mod graph_access;
mod handoff;
pub mod headless;
mod interview;
mod issue_panel;
mod kept_worktrees;
pub mod layout;
mod local_build;
mod machines;
mod new_project;
mod parking;
pub mod process_memory;
mod project_health;
mod projects;
mod proofs_settings;
mod provider_agents;
mod providers;
mod ready_pool;
mod release;
mod restart;
mod rosen;
mod rosen_events;
mod rosen_issue;
mod rosen_landing;
mod rosen_migration;
mod rosen_mirror;
mod rosen_pin;
mod rosen_rebase;
mod rosen_status;
mod rosen_thread;
mod shared_mcp;
mod skills;
mod state;
mod swarm;
mod swarm_deletion;
pub mod sweeps;
mod tasks;
mod thread_links;
mod thread_menu;
mod threads;
mod token_usage;
mod updates;
mod velocity_reports;
mod window;
mod work_pool;
mod workspace_settings;

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
        browser::browser_input,
        browser::close_browser_view,
        browser::hand_back_browser,
        browser::open_browser_view,
        browser::switch_browser_tab,
        browser::take_over_browser,
        code_graph::get_code_graph,
        code_graph::set_code_graph,
        code_graph::reset_code_graph_usage,
        code_graph::graph_status,
        code_graph::retry_graph_index,
        collapse::open_on_collapse,
        collapse_connection::connect_collapse,
        collapse_connection::disconnect_collapse,
        collapse_connection::get_collapse_connection,
        collapse_connection::set_collapse_mcp,
        collapse_mirror::mirror_to_collapse,
        collapse_mirror::send_collapse_live_state,
        continuous_dispatch::get_continuous_dispatch,
        continuous_dispatch::set_continuous_dispatch,
        token_usage::get_token_usage,
        window::get_window_state,
        window::minimize_window,
        window::toggle_maximize_window,
        window::close_window,
        window::window_heartbeat,
        diagnostics::log_window_error,
        process_memory::get_process_memory,
        project_health::get_project_health,
        state::get_settings,
        state::set_settings,
        state::get_ui_zoom,
        state::adjust_ui_zoom,
        layout::get_layout_state,
        layout::dispatch_layout,
        local_build::get_local_build_status,
        local_build::restart_into_local_build,
        machines::get_machines,
        machines::enable_machine_listener,
        machines::create_machine_join_key,
        machines::approve_machine,
        machines::revoke_machine,
        machines::dispatch_to_machine,
        projects::list_projects,
        projects::switch_project,
        release::cut_release,
        proofs_settings::get_proofs_settings,
        provider_agents::set_provider_command,
        provider_agents::add_custom_agent,
        provider_agents::remove_custom_agent,
        providers::list_providers,
        providers::install::install_provider,
        providers::accounts::edit_provider_account,
        providers::provider_sign_in,
        providers::provider_sign_out,
        providers::set_provider_settings,
        providers::get_model_menu,
        providers::choose_model,
        work_pool::get_work_pool,
        work_pool::get_work_change,
        workspace_settings::get_workspace_settings,
        workspace_settings::set_workspace_settings,
        workspace_settings::preview_workspace_folder,
        workspace_settings::browse_workspace_folder,
        workspace_settings::list_workspaces,
        workspace_settings::delete_workspace,
        workspace_settings::get_workspace_preparation,
        workspace_settings::propose_workspace_preparation,
        duty::get_duty_dialog_defaults,
        duty::get_duty_agent_transcript,
        duty::start_duty,
        duty::pause_duty,
        duty::add_duty_agents,
        duty::dismiss_duty_agent,
        duty::stop_duty,
        goal_swarm::get_goal_swarm_dialog,
        goal_swarm::read_goal_swarm_pool,
        goal_swarm::launch_goal_swarm,
        goal_swarm::add_goal_agents,
        goal_swarm::get_goal_swarm_board,
        issue_panel::get_issue_panel,
        issue_panel::get_issue_discussion,
        issue_panel::open_issue_discussion,
        issue_panel::open_project_issue,
        issue_panel::release_issue_hold,
        issue_panel::dispatch_issue,
        issue_panel::dispatch_work_issue,
        issue_panel::set_work_issue_expanded,
        issue_panel::toggle_issue_expanded,
        rosen_mirror::adopt_mirror_tip,
        rosen_mirror::catch_up_from_mirror,
        rosen_status::switch_to_after_land,
        rosen_status::watch_rosen_status,
        rosen_thread::run_rosen_thread_action,
        shared_mcp::edit_shared_mcp_servers,
        skills::get_skill_library,
        skills::get_skill_pack_installations,
        skills::get_skill_settings,
        skills::set_skill_settings,
        skills::set_skill_bundle_default,
        skills::remove_skill_pack,
        skills::install_skill_pack,
        skills::check_skill_pack_update,
        skills::confirm_skill_pack_update,
        skills::generate_design_system,
        swarm::get_swarm_dialog,
        swarm::launch_swarm,
        swarm::add_to_swarm,
        swarm_deletion::delete_swarm,
        tasks::add_task,
        tasks::undo_task_event,
        tasks::list_tasks,
        thread_menu::get_thread_menu,
        thread_menu::run_thread_menu_action,
        thread_menu::delete_thread,
        thread_menu::undo_delete_thread,
        updates::get_update_status,
        updates::check_for_update,
        updates::install_update,
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
        .typ::<browser::BrowserFrame>()
        .typ::<collapse::CollapseLinkOpened>()
        .typ::<duty::DutyEventPublished>()
        .typ::<window::WindowStateChanged>()
        .typ::<layout::LayoutStateChanged>()
        .typ::<layout::LayoutIntentRequested>()
        .typ::<local_build::LocalBuildStatusChanged>()
        .typ::<project_health::ProjectHealthChanged>()
        .typ::<projects::ProjectSelected>()
        .typ::<providers::ProvidersChanged>()
        .typ::<swarm::SwarmEventPublished>()
        .typ::<issue_panel::IssuePanelChanged>()
        .typ::<rosen_status::RosenStatusChanged>()
        .typ::<skills::SkillPackInstallationsChanged>()
        .typ::<threads::ThreadStatusChanged>()
        .typ::<threads::ThreadStepRecorded>()
        .typ::<threads::ThreadSubagentChanged>()
        .typ::<updates::UpdateStatusChanged>()
        .constant("AIRC_EVENT", airc_events::AircEventPublished::NAME)
        .constant(
            "AIRC_MESSAGE_EVENT",
            airc_events::AircMessagePublished::NAME,
        )
        .constant("AIRC_MODAL_EVENT", airc_modal::AircModalPublished::NAME)
        .constant("BROWSER_FRAME_EVENT", browser::BrowserFrame::NAME)
        .constant("COLLAPSE_LINK_EVENT", collapse::CollapseLinkOpened::NAME)
        .constant("DUTY_EVENT", duty::DutyEventPublished::NAME)
        .constant("PROJECT_SELECTED_EVENT", projects::ProjectSelected::NAME)
        .constant("PROVIDERS_EVENT", providers::ProvidersChanged::NAME)
        .constant("SWARM_EVENT", swarm::SwarmEventPublished::NAME)
        .constant("ISSUE_PANEL_EVENT", issue_panel::IssuePanelChanged::NAME)
        .constant("WINDOW_STATE_EVENT", window::WindowStateChanged::NAME)
        .constant("LAYOUT_STATE_EVENT", layout::LayoutStateChanged::NAME)
        .constant("LAYOUT_INTENT_EVENT", layout::LayoutIntentRequested::NAME)
        .constant(
            "LOCAL_BUILD_STATUS_EVENT",
            local_build::LocalBuildStatusChanged::NAME,
        )
        .constant(
            "PROJECT_HEALTH_EVENT",
            project_health::ProjectHealthChanged::NAME,
        )
        .constant("ROSEN_STATUS_EVENT", rosen_status::RosenStatusChanged::NAME)
        .constant(
            "SKILL_PACK_INSTALLATIONS_EVENT",
            skills::SkillPackInstallationsChanged::NAME,
        )
        .constant("THREAD_STATUS_EVENT", threads::ThreadStatusChanged::NAME)
        .constant("THREAD_STEP_EVENT", threads::ThreadStepRecorded::NAME)
        .constant(
            "THREAD_SUBAGENT_EVENT",
            threads::ThreadSubagentChanged::NAME,
        )
        .constant("UPDATE_STATUS_EVENT", updates::UpdateStatusChanged::NAME)
        .error_handling(tauri_specta::ErrorHandlingMode::Throw)
}

/// A warm-pool exporter must write into the tree being checked, even when its
/// binary was compiled in the template. The debug app can also start outside a
/// checkout, where its original manifest still identifies its source tree.
fn bindings_path() -> std::path::PathBuf {
    if let Ok(directory) = std::env::current_dir()
        && let Some(root) = directory.ancestors().find(|path| {
            path.join("src-tauri/Cargo.toml").is_file() && path.join("ui/src/lib").is_dir()
        })
    {
        return root.join("ui/src/lib/bindings.ts");
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/src/lib/bindings.ts")
}

/// Regenerates the bindings and fails when the copy on disk was stale. This is the check
/// for a tree with no git to diff against, such as a Rosen merge result. Line endings are
/// ignored: a git checkout on Windows can hold the file with CRLF.
pub fn check_bindings() -> anyhow::Result<()> {
    let unix = |text: String| text.replace("\r\n", "\n");
    let committed = std::fs::read_to_string(bindings_path()).unwrap_or_default();
    export_bindings()?;
    let generated = std::fs::read_to_string(bindings_path())?;
    anyhow::ensure!(
        unix(committed) == unix(generated),
        "ui/src/lib/bindings.ts was stale. It is regenerated now: include it in the change."
    );
    Ok(())
}

pub fn export_bindings() -> anyhow::Result<()> {
    let destination = bindings_path();
    bindings()
        .export(specta_typescript::Typescript::default(), &destination)
        .context("Could not generate the frontend command and event bindings")?;
    // Keep generated output deterministic without a trailing blank line.
    let generated = std::fs::read_to_string(&destination)?;
    std::fs::write(destination, format!("{}\n", generated.trim_end()))
        .context("Could not normalize the generated bindings")
}

fn application_environment() -> tauri::Env {
    let mut environment = tauri::Env::default();
    // Tauri's updater forwards these arguments to both setup and MSI replacements.
    environment.args_os = std::env::args_os()
        .take(1)
        .chain(velocity_updates::supervision::application_arguments())
        .collect();
    environment
}

pub fn run() -> anyhow::Result<()> {
    let recovery =
        std::sync::Arc::new(velocity_updates::supervision::RecoveryConnection::connect()?);
    let setup_recovery = recovery.clone();
    #[cfg(debug_assertions)]
    export_bindings()?;
    updates::wait_after_portable_update();

    let bindings = bindings().events(collect_events![
        airc_events::AircEventPublished,
        airc_events::AircMessagePublished,
        airc_modal::AircModalPublished,
        browser::BrowserFrame,
        collapse::CollapseLinkOpened,
        duty::DutyEventPublished,
        window::WindowStateChanged,
        layout::LayoutStateChanged,
        layout::LayoutIntentRequested,
        local_build::LocalBuildStatusChanged,
        project_health::ProjectHealthChanged,
        projects::ProjectSelected,
        providers::ProvidersChanged,
        swarm::SwarmEventPublished,
        issue_panel::IssuePanelChanged,
        rosen_status::RosenStatusChanged,
        skills::SkillPackInstallationsChanged,
        threads::ThreadStatusChanged,
        threads::ThreadStepRecorded,
        threads::ThreadSubagentChanged,
        updates::UpdateStatusChanged
    ]);
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |application, arguments, _| {
                for argument in arguments {
                    thread_links::open(application, &argument);
                }
            },
        ))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(application_environment())
        .manage(recovery.clone())
        .manage(agents::ManagedAgents::default())
        .manage(collapse::PendingCollapseLink::default())
        .manage(goal_swarm::GoalSwarmPool::default())
        .manage(layout::MainLayout::default())
        .manage(new_project::ManagedRepositoryPicker::default())
        .manage(providers::ManagedProviders::default())
        .manage(airc_modal::ManagedAircModal::default())
        .manage(rosen_status::StatusWatch::default())
        .manage(threads::ManagedThreads::default())
        .manage(velocity_interview::Interviews::default())
        .manage(thread_menu::ManagedDeletions::default())
        .manage(window::WindowHeartbeat::default())
        .invoke_handler(bindings.invoke_handler())
        .setup(move |application| {
            setup_recovery.set_intentional_exit(false)?;
            bindings.mount_events(application);
            diagnostics::initialize(application.handle());
            if let Some(notice) = setup_recovery.take_restart_notice() {
                diagnostics::record("recovery", &notice);
            }
            let interrupted = state::initialize(application.handle())?;
            browser::initialize(application.handle())?;
            airc_access::initialize(application.handle())?;
            projects::initialize(application.handle())?;
            providers::initialize(application.handle());
            rosen::initialize(application.handle())?;
            rosen_mirror::initialize(application.handle());
            collapse_mirror::initialize(application.handle());
            kept_worktrees::initialize(application.handle());
            github_requests::initialize(application.handle());
            issue_panel::initialize(application.handle());
            continuous_dispatch::initialize(application.handle());
            workspace_settings::initialize(application.handle());
            ready_pool::initialize(application.handle())?;
            skills::initialize(application.handle())?;
            interview::start(application.handle());
            rosen_landing::initialize(application.handle())?;
            base_ci::initialize(application.handle());
            project_health::initialize(application.handle());
            sweeps::initialize(application.handle())?;
            airc_delivery::initialize(application.handle());
            airc_modal::initialize(application.handle());
            updates::initialize(application.handle());
            local_build::initialize(application.handle());
            rosen_pin::initialize(application.handle());
            velocity_reports::initialize(application.handle());
            restart::resume_interrupted(application.handle(), interrupted);
            rosen_migration::initialize(application.handle());
            thread_links::initialize(application.handle());
            window::watch_renderer(application.handle());
            process_memory::start(application.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            window::on_window_event(window, event);
        });
    // The single-instance plugin exits before setup when it hands off a URL or launch.
    recovery.set_intentional_exit(true)?;
    let application = builder
        .build(tauri::generate_context!())
        .inspect_err(|_| {
            let _ = recovery.set_intentional_exit(false);
        })
        .context("Could not run Velocity")?;
    application.run(|application, event| {
        // Nothing can stop the exit now; a failed save is logged like any other.
        if matches!(event, tauri::RunEvent::Exit) {
            use tauri::Manager;
            if let Err(error) = application
                .state::<std::sync::Arc<velocity_updates::supervision::RecoveryConnection>>()
                .finish_intentional_exit()
            {
                diagnostics::record("recovery", &error.to_string());
            }
            if let Err(error) = state::save_now(application) {
                eprintln!("{error}");
            }
            rosen::shutdown(application);
        }
    });
    Ok(())
}
