//! Open on Collapse (#429) and Open in Velocity (#479).
//!
//! Outbound: Rust builds the Collapse repository or land URL from the saved Collapse mirror,
//! so the window never names an address and projects without one do nothing.
//! Inbound: Rust parses `velocity://collapse/<owner>/<name>` (and optional `/land/<commit>`),
//! focuses the matching project with a fresh unsent draft thread or emits an unmatched notice,
//! and never passes link text into a prompt or a shell.

use std::sync::Mutex;

use tauri::{AppHandle, Manager, WebviewWindow};
use tauri_plugin_opener::OpenerExt;
use tauri_specta::Event;
use velocity_core::collapse::CollapseDeepLink;
use velocity_core::layout_view::LayoutIntent;

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// `collapse://link` (§3.1): emitted when a `velocity://collapse/...` deep link arrives (#479).
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type, Event,
)]
#[tauri_specta(event_name = "collapse://link")]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CollapseLinkOpened {
    /// Matched a local project whose Collapse mirror is `repository`; Rust focused that
    /// project and opened an unsent draft thread.
    Opened {
        project_identifier: u32,
        repository: String,
        draft_context: Option<String>,
    },
    /// No local project mirrors `repository`; Rust changed nothing and the window shows
    /// `notice`, offering nothing else.
    Unmatched { repository: String, notice: String },
}

/// Holds a cold-start `velocity://collapse/...` event until the window has mounted its
/// listeners and fetched the initial layout state, so startup links are never dropped.
#[derive(Default)]
pub struct PendingCollapseLink(Mutex<PendingLinkState>);

#[derive(Default)]
struct PendingLinkState {
    window_ready: bool,
    queued: Option<CollapseLinkOpened>,
}

/// `open_on_collapse` (§3.1): `commit` names a land; without it the repository page opens.
#[tauri::command]
#[specta::specta]
pub fn open_on_collapse(
    application: AppHandle,
    project_identifier: u32,
    commit: Option<String>,
) -> Result<(), String> {
    let repository = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project_identifier)
    .and_then(|project| project.collapse_repository.clone())
    .ok_or("This project has no Collapse mirror")?;
    open_page(&application, &repository, commit.as_deref())
}

/// The palette's Open on Collapse entries act on the active project.
pub(crate) fn handle_intent(application: &AppHandle, intent: &LayoutIntent) -> bool {
    let LayoutIntent::OpenOnCollapse(commit) = intent else {
        return false;
    };
    let repository = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .map(|store| {
        store
            .active_project()
            .and_then(|project| project.collapse_repository.clone())
    });
    let opened = match repository {
        Ok(Some(repository)) => open_page(application, &repository, commit.as_deref()),
        Ok(None) => Err("This project has no Collapse mirror".into()),
        Err(error) => Err(error),
    };
    crate::threads::report(opened);
    true
}

fn open_page(
    application: &AppHandle,
    repository: &str,
    commit: Option<&str>,
) -> Result<(), String> {
    let page = match commit {
        Some(commit) => velocity_core::collapse::land_page(repository, commit),
        None => velocity_core::collapse::repository_page(repository),
    }
    .ok_or("That is not a Collapse repository or landed commit")?;
    application
        .opener()
        .open_url(page, None::<&str>)
        .map_err(|error| error.to_string())
}

/// Handles a `velocity://collapse/...` deep link (#479). Returns `true` when `argument`
/// parsed as a Collapse link, `false` for anything else. Never starts an agent or shell.
pub(crate) fn open_link(application: &AppHandle, argument: &str) -> bool {
    let Some(link) = velocity_core::collapse::parse_deep_link(argument) else {
        return false;
    };
    report(handle_link(application, link));
    true
}

/// Marks the main window ready for `collapse://link` events and emits any cold-start link
/// that arrived before the webview mounted.
pub(crate) fn flush_pending_link(application: &AppHandle) {
    let Some(pending) = application.try_state::<PendingCollapseLink>() else {
        return;
    };
    let queued = match pending.0.lock() {
        Ok(mut state) => {
            state.window_ready = true;
            state.queued.take()
        }
        Err(_) => return,
    };
    if let (Some(event), Some(window)) = (queued, application.get_webview_window("main")) {
        report(event.emit(&window).map_err(|error| error.to_string()));
    }
}

fn handle_link(application: &AppHandle, link: CollapseDeepLink) -> Result<(), String> {
    let Some(window) = application.get_webview_window("main") else {
        return Ok(());
    };
    let Some(store_state) = application.try_state::<ManagedStateStore>() else {
        return Ok(());
    };
    let (event, publish_bundle) = {
        let mut store = lock(store_state.inner(), "persisted state")?;
        let matched = store
            .state()
            .projects
            .iter()
            .find(|project| {
                project
                    .collapse_repository
                    .as_deref()
                    .is_some_and(|saved| saved.eq_ignore_ascii_case(&link.repository))
            })
            .map(|project| project.id);
        match matched {
            Some(project_identifier) => {
                let (event, snapshot, selection) =
                    open_matched_project(application, &mut store, project_identifier, link)?;
                (event, Some((snapshot, selection)))
            }
            None => (
                CollapseLinkOpened::Unmatched {
                    notice: velocity_core::collapse::unmatched_project_notice(&link.repository),
                    repository: link.repository,
                },
                None,
            ),
        }
    };
    if let Some((snapshot, selection)) = publish_bundle {
        crate::layout::publish(&window, &snapshot, None)?;
        if let Some((projects, selection)) = selection {
            crate::projects::publish_selection(&window.as_ref().window(), projects, selection)?;
        }
    }
    if let CollapseLinkOpened::Opened {
        project_identifier, ..
    } = &event
    {
        crate::skills::recheck_suggestions(
            application,
            *project_identifier,
            crate::skills::SuggestionCheck::Opened,
        );
    }
    deliver_link_event(application, &window, event)?;
    focus_window(&window)
}

type MatchedProjectPublication = (
    CollapseLinkOpened,
    velocity_core::layout_view::LayoutSnapshot,
    Option<(
        velocity_store::projects::ProjectList,
        velocity_store::projects::ProjectSelectionView,
    )>,
);

fn open_matched_project(
    application: &AppHandle,
    store: &mut velocity_store::StateStore,
    project_identifier: u32,
    link: CollapseDeepLink,
) -> Result<MatchedProjectPublication, String> {
    let layout = application.state::<MainLayout>();
    let mut layout = lock(layout.inner(), "main-window state")?;
    let selection = if store
        .active_project()
        .is_some_and(|active| active.id == project_identifier)
    {
        None
    } else {
        Some(
            store
                .switch_project_layout(&mut layout, project_identifier)
                .map_err(|error| error.to_string())?,
        )
    };
    let draft_context = link
        .commit
        .as_deref()
        .and_then(velocity_core::collapse::land_context_label);
    layout.open_draft_thread(draft_context.clone());
    let snapshot = layout.snapshot();
    let selection = selection.map(|selection| (store.list_projects(), selection));
    Ok((
        CollapseLinkOpened::Opened {
            project_identifier,
            repository: link.repository,
            draft_context,
        },
        snapshot,
        selection,
    ))
}

fn deliver_link_event(
    application: &AppHandle,
    window: &WebviewWindow,
    event: CollapseLinkOpened,
) -> Result<(), String> {
    if let Some(pending) = application.try_state::<PendingCollapseLink>() {
        let mut state = lock(&pending.0, "pending collapse link")?;
        if !state.window_ready {
            state.queued = Some(event);
            return Ok(());
        }
    }
    event.emit(window).map_err(|error| error.to_string())
}

fn focus_window(window: &WebviewWindow) -> Result<(), String> {
    window.unminimize().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())
}
