use anyhow::Context;
use std::path::Path;
use tauri_specta::{Builder, Event, collect_commands, collect_events};

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
        ])
        .typ::<window::WindowStateChanged>()
        .constant("WINDOW_STATE_EVENT", window::WindowStateChanged::NAME)
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

    let bindings = bindings().events(collect_events![window::WindowStateChanged]);
    tauri::Builder::default()
        .invoke_handler(bindings.invoke_handler())
        .setup(move |application| {
            bindings.mount_events(application);
            Ok(())
        })
        .on_window_event(window::on_window_event)
        .run(tauri::generate_context!())
        .context("Could not run Velocity")
}
