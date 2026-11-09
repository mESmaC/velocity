use anyhow::Context;
use std::path::Path;
use tauri_specta::{Builder, Event, collect_commands, collect_events};
use velocity_store::PersistedState;

mod build_gate;
mod layout;
mod projects;
mod state;
mod window;

fn bindings() -> Builder<tauri::Wry> {
    // Export event payloads/names without the generator's `any`-typed helpers;
    // ipc.ts supplies typed listeners, while run() registers native emitters.
    Builder::new()
        .commands(collect_commands![
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
            build_gate::get_launch_eligibility,
        ])
        .typ::<PersistedState>()
        .typ::<window::WindowStateChanged>()
        .typ::<layout::LayoutStateChanged>()
        .typ::<layout::LayoutIntentRequested>()
        .typ::<projects::ProjectSelected>()
        .constant("WINDOW_STATE_EVENT", window::WindowStateChanged::NAME)
        .constant("LAYOUT_STATE_EVENT", layout::LayoutStateChanged::NAME)
        .constant("LAYOUT_INTENT_EVENT", layout::LayoutIntentRequested::NAME)
        .constant("PROJECT_SELECTED_EVENT", projects::ProjectSelected::NAME)
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
        projects::ProjectSelected
    ]);
    tauri::Builder::default()
        .manage(layout::MainLayout::default())
        .invoke_handler(bindings.invoke_handler())
        .setup(move |application| {
            bindings.mount_events(application);
            state::initialize(application.handle())?;
            projects::initialize(application.handle())?;
            Ok(())
        })
        .on_window_event(window::on_window_event)
        .run(tauri::generate_context!())
        .context("Could not run Velocity")
}
