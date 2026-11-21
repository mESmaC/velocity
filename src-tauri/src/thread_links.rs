use tauri::{AppHandle, Manager};
use tauri_plugin_deep_link::DeepLinkExt;
use velocity_core::layout_view::LayoutAction;

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

pub fn initialize(application: &AppHandle) {
    let handle = application.clone();
    application.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            open(&handle, url.as_str());
        }
    });
    for argument in velocity_updates::supervision::application_arguments() {
        if let Some(argument) = argument.to_str() {
            open(application, argument);
        }
    }
}

pub fn open(application: &AppHandle, argument: &str) {
    if crate::collapse::open_link(application, argument) {
        return;
    }
    let Some((project, thread)) = argument
        .strip_prefix("velocity://thread/")
        .and_then(|path| path.split_once('/'))
        .and_then(|(project, thread)| {
            Some((project.parse::<u32>().ok()?, thread.parse::<u32>().ok()?))
        })
    else {
        return;
    };
    report(show_thread(application, project, thread));
}

fn show_thread(application: &AppHandle, project: u32, thread: u32) -> Result<(), String> {
    let Some(window) = application.get_webview_window("main") else {
        return Ok(());
    };
    let Some(store) = application.try_state::<ManagedStateStore>() else {
        return Ok(());
    };
    let (snapshot, selection) = {
        let mut store = lock(store.inner(), "persisted state")?;
        if !store
            .project(project)
            .is_some_and(|project| project.threads.iter().any(|item| item.id == thread))
        {
            return Err("The linked Velocity thread is unavailable on this installation".into());
        }
        let layout = application.state::<MainLayout>();
        let mut layout = lock(layout.inner(), "main-window state")?;
        let selection = if store
            .active_project()
            .is_some_and(|active| active.id == project)
        {
            None
        } else {
            Some(
                store
                    .switch_project_layout(&mut layout, project)
                    .map_err(|error| error.to_string())?,
            )
        };
        let outcome = store
            .dispatch_project_layout(&mut layout, LayoutAction::OpenThread(thread as usize))
            .map_err(|error| error.to_string())?;
        let selection = selection.map(|selection| (store.list_projects(), selection));
        (outcome.snapshot, selection)
    };
    crate::layout::publish(&window, &snapshot, None)?;
    if let Some((projects, selection)) = selection {
        crate::projects::publish_selection(&window.as_ref().window(), projects, selection)?;
    }
    window.unminimize().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}
