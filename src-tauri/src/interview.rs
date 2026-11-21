//! Spec interview wiring (SPEC.md F3): hands `velocity-interview` the managed state, the
//! `layout://state` emitter and Tauri's async runtime.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use agent_client_protocol::v1::schema::McpServer;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_acp::client::{AgentLaunch, AircLaunch, SessionSetup};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutIntent, LayoutSnapshot};
use velocity_interview::{Host, InterviewKey, Interviews};
use velocity_store::StateStore;

use crate::layout::{LayoutStateChanged, MainLayout};
use crate::state::ManagedStateStore;

pub struct AppHost(AppHandle);

impl Host for AppHost {
    fn store(&self) -> &Mutex<StateStore> {
        self.0.state::<ManagedStateStore>().inner()
    }

    fn layout(&self) -> &Mutex<LayoutState> {
        self.0.state::<MainLayout>().inner()
    }

    fn interviews(&self) -> &Interviews {
        self.0.state::<Interviews>().inner()
    }

    fn publish(&self, snapshot: LayoutSnapshot) {
        if let Err(error) = (LayoutStateChanged { state: snapshot }).emit(&self.0) {
            eprintln!("Could not publish main-window state: {error}");
        }
    }

    fn shared_mcp_servers(&self, settings: &velocity_store::Settings) -> Vec<McpServer> {
        velocity_threads::shared_mcp_servers(settings)
    }

    fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send>>) {
        tauri::async_runtime::spawn(task);
    }

    fn prepare<'a>(
        &'a self,
        key: InterviewKey,
        launch: &'a mut AgentLaunch,
        setup: &'a mut SessionSetup,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if crate::rosen::uses_rosen(&self.0, key.0) {
                prepare_workspace(&self.0, key, launch, setup).await?;
            }
            Ok(())
        })
    }

    fn publish_spec<'a>(
        &'a self,
        key: InterviewKey,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(publish_spec(&self.0, key))
    }

    fn airc(&self) -> Option<AircLaunch> {
        crate::airc_access::managed(&self.0)
    }
}

fn host(application: &AppHandle) -> Arc<AppHost> {
    Arc::new(AppHost(application.clone()))
}

/// Starts the interviews Create recorded, and resumes ones a restart cut off.
pub fn start(application: &AppHandle) {
    velocity_interview::start_interviews(&host(application));
    // The scan also suggests skill packs for the project (F3 step 1, F18).
    for project in scanning_projects(application) {
        crate::skills::recheck_suggestions(
            application,
            project,
            crate::skills::SuggestionCheck::Opened,
        );
    }
}

fn scanning_projects(application: &AppHandle) -> Vec<u32> {
    let state = application.state::<ManagedStateStore>();
    let Ok(store) = state.lock() else {
        return Vec::new();
    };
    store
        .state()
        .projects
        .iter()
        .filter(|project| {
            project.threads.iter().any(|thread| {
                thread
                    .interview
                    .as_ref()
                    .is_some_and(|record| record.stage == velocity_store::InterviewStage::Scan)
            })
        })
        .map(|project| project.id)
        .collect()
}

/// Carries out `intent` when it belongs to the open spec interview.
pub fn handle_intent(application: &AppHandle, intent: &LayoutIntent) -> bool {
    velocity_interview::handle_intent(&host(application), intent)
}

struct InterviewWorkspace {
    repository: PathBuf,
    root: PathBuf,
    change: Option<String>,
    spec_path: String,
}

fn workspace(application: &AppHandle, key: InterviewKey) -> Result<InterviewWorkspace, String> {
    let state = application.state::<ManagedStateStore>();
    let store = state.lock().map_err(|error| error.to_string())?;
    let project = store.project(key.0).ok_or("Unknown project")?;
    let thread = project
        .threads
        .iter()
        .find(|thread| thread.id == key.1)
        .ok_or("Unknown interview")?;
    let repository = PathBuf::from(&project.repository_path);
    let root = thread.worktree_path.as_ref().map_or_else(
        || {
            repository
                .join(".rosen")
                .join("interviews")
                .join(format!("spec-{}", key.1))
        },
        PathBuf::from,
    );
    Ok(InterviewWorkspace {
        repository,
        root,
        change: thread.rosen_change.clone(),
        spec_path: project.spec_path.clone(),
    })
}

async fn prepare_workspace(
    application: &AppHandle,
    key: InterviewKey,
    launch: &mut AgentLaunch,
    setup: &mut SessionSetup,
) -> Result<(), String> {
    let server = crate::rosen::server(application, key.0).await?;
    let workspace = workspace(application, key)?;
    let agent = velocity_rosen::agent_name(&workspace.root);
    let change = if let Some(change) = workspace.change {
        velocity_rosen::reopen_workspace(&server, &agent, &change, &workspace.root)
            .await
            .map_err(|error| error.to_string())?;
        change
    } else {
        let created = velocity_rosen::create_workspace(&server, &agent, &workspace.root, None)
            .await
            .map_err(|error| error.to_string())?;
        let state = application.state::<ManagedStateStore>();
        let recorded = state
            .lock()
            .map_err(|error| error.to_string())
            .and_then(|mut store| {
                store
                    .update_thread(key.0, key.1, |thread| {
                        thread.worktree_path = Some(created.root.to_string_lossy().into_owned());
                        thread.rosen_change = Some(created.change.clone());
                    })
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = recorded {
            let _ = velocity_rosen::abandon_change(&server, &agent, &created.change).await;
            return Err(error);
        }
        created.change
    };
    crate::rosen::watch(application, &server, &workspace.root).await?;
    let environment = velocity_rosen::agent_environment(&server, &agent, &change);
    if let Some(mcp) = crate::rosen::mcp_server(&workspace.root, &environment) {
        launch.shared_servers.push(mcp);
    }
    launch.env.extend(environment);
    setup.session_card = Some(velocity_acp::client::SessionCard {
        identity: Some(format!("{agent} (spec-{})", key.1)),
        workspace: Some(workspace.root.clone()),
        change: Some(change),
        is_rosen: true,
        unavailable_tools: vec!["velocity-browser (spec interview)".into()],
        ..velocity_acp::client::SessionCard::default()
    });
    setup.worktree = workspace.root;
    Ok(())
}

async fn publish_spec(application: &AppHandle, key: InterviewKey) -> Result<(), String> {
    let server = crate::rosen::server(application, key.0).await?;
    let workspace = workspace(application, key)?;
    let change = workspace.change.ok_or("Interview workspace missing")?;
    // The active watcher journals in the workspace that land removes.
    crate::rosen::stop_watch(application, &workspace.root).await;
    let result = velocity_rosen::project::publish_spec(
        &server,
        &workspace.root,
        &workspace.repository,
        &change,
        &workspace.spec_path,
    )
    .await;
    if result.is_err() && workspace.root.is_dir() {
        let _ = crate::rosen::watch(application, &server, &workspace.root).await;
    }
    result.map_err(|error| error.to_string())
}
