//! Providers (SPEC.md F15): the live sign-in state of each provider's agent, sign-in and
//! sign-out, subscription limits, Settings → Providers and the composer's model menu.
//!
//! Each agent is asked at startup and after every sign-in; limits are read every five
//! minutes. What the agents report is saved in the store's provider catalog, so pickers and
//! the footer meter are right at the next start before the agents answer again.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_acp::client::{
    AgentProbe, ClientError, probe_agent, read_claude_usage, read_codex_usage, sign_in,
};
use velocity_acp::{LimitReading, SignInMethod, agent_command_label, choose_sign_in};
use velocity_store::provider_views::{
    MenuTarget, ModelMenuChoice, ModelMenuView, ProviderContext, ProviderPhase, ProviderRuntime,
    ProvidersView,
};
use velocity_store::providers::{
    PROVIDERS, ProviderEdit, ProviderLimit, ProviderModel, provider_facts, unix_now,
};
use velocity_store::{ProviderKind, StateStore};
use velocity_threads::{ThreadRuntimes, agent_launch, thread_run_settings};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::ManagedThreads;

/// Starting an agent and reading its models takes a few seconds (Claude Code's took three).
const PROBE_DEADLINE: Duration = Duration::from_secs(90);
/// Long enough to sign in in the browser, short enough that a forgotten tab ends.
const SIGN_IN_DEADLINE: Duration = Duration::from_secs(10 * 60);
const LIMIT_DEADLINE: Duration = Duration::from_secs(60);
const LIMIT_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, serde::Serialize, specta::Type, Event)]
#[tauri_specta(event_name = "provider://status")]
pub struct ProvidersChanged {
    pub view: ProvidersView,
}

/// One provider's live state in the running app.
struct Slot {
    runtime: ProviderRuntime,
    /// The sign-in method its agent offered that Velocity may use.
    method: Option<SignInMethod>,
    sign_in: Option<JoinHandle<()>>,
}

pub struct ManagedProviders(Mutex<Vec<Slot>>);

impl Default for ManagedProviders {
    fn default() -> Self {
        let slots = PROVIDERS
            .into_iter()
            .map(|provider| Slot {
                runtime: ProviderRuntime::new(
                    provider,
                    agent_command_label(&agent_launch(provider).0),
                ),
                method: None,
                sign_in: None,
            })
            .collect();
        Self(Mutex::new(slots))
    }
}

impl ManagedProviders {
    fn with_slot<T>(
        &self,
        provider: ProviderKind,
        change: impl FnOnce(&mut Slot) -> T,
    ) -> Option<T> {
        let mut slots = self.0.lock().ok()?;
        slots
            .iter_mut()
            .find(|slot| slot.runtime.provider == provider)
            .map(change)
    }

    fn runtime(&self) -> Vec<ProviderRuntime> {
        self.0
            .lock()
            .map(|slots| slots.iter().map(|slot| slot.runtime.clone()).collect())
            .unwrap_or_default()
    }

    fn phase(&self, provider: ProviderKind) -> Option<ProviderPhase> {
        self.with_slot(provider, |slot| slot.runtime.phase)
    }
}

pub fn initialize(application: &AppHandle) {
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        let checks = PROVIDERS.map(|provider| {
            let handle = handle.clone();
            tauri::async_runtime::spawn(async move { check(&handle, provider).await })
        });
        for check in checks {
            let _ = check.await;
        }
        loop {
            tokio::time::sleep(LIMIT_INTERVAL).await;
            refresh_limits(&handle).await;
        }
    });
}

/// Asks `provider`'s agent for its sign-in state and models, then its limit.
async fn check(application: &AppHandle, provider: ProviderKind) {
    let (launch, _) = agent_launch(provider);
    let result = probe_agent(&launch, &working_directory(application), PROBE_DEADLINE).await;
    let probe = match result {
        Ok(probe) => probe,
        Err(error) => {
            record_failure(application, provider, &error);
            publish(application, None);
            return;
        }
    };
    record_probe(application, provider, &probe);
    if probe.signed_in {
        read_limit(application, provider).await;
    }
    publish(application, None);
}

fn record_probe(application: &AppHandle, provider: ProviderKind, probe: &AgentProbe) {
    let method = choose_sign_in(&probe.sign_in_methods)
        .and_then(|index| probe.sign_in_methods.get(index))
        .cloned();
    application
        .state::<ManagedProviders>()
        .with_slot(provider, |slot| {
            slot.runtime.phase = if probe.signed_in {
                ProviderPhase::SignedIn
            } else {
                ProviderPhase::SignedOut
            };
            slot.runtime.can_sign_in = method.is_some();
            slot.runtime.detail = None;
            slot.method = method;
        });
    let models = probe
        .models
        .iter()
        .map(|model| ProviderModel::new(model.id.clone(), model.name.clone(), model.fast))
        .collect();
    with_store(application, |store| {
        store.record_provider_probe(provider, probe.signed_in, models)
    });
}

fn record_failure(application: &AppHandle, provider: ProviderKind, error: &ClientError) {
    let missing = matches!(
        error,
        ClientError::Spawn { source, .. } if source.kind() == std::io::ErrorKind::NotFound
    );
    application
        .state::<ManagedProviders>()
        .with_slot(provider, |slot| {
            slot.runtime.phase = if missing {
                ProviderPhase::NotInstalled
            } else {
                ProviderPhase::Failed
            };
            slot.runtime.detail = (!missing).then(|| error.to_string());
        });
    // An agent that can't start can't run work: its models leave the pickers.
    with_store(application, |store| {
        store.record_provider_probe(provider, false, Vec::new())
    });
}

/// Reads one provider's limit where it has a source (F15 Limit sources). A failed read shows
/// "limit not reported".
async fn read_limit(application: &AppHandle, provider: ProviderKind) {
    let directory = working_directory(application);
    let reading = match provider {
        ProviderKind::Claude => read_claude_usage(&directory, LIMIT_DEADLINE).await,
        ProviderKind::Codex => read_codex_usage(&directory, LIMIT_DEADLINE).await,
        ProviderKind::Antigravity | ProviderKind::Kimi => Ok(None),
    };
    let reading: Option<LimitReading> = reading.ok().flatten();
    let plan = reading.as_ref().and_then(|reading| reading.plan.clone());
    let limit = reading.map(|reading| {
        ProviderLimit::new(
            reading.percent_used,
            reading.window,
            reading
                .resets_at
                .and_then(|seconds| u32::try_from(seconds).ok()),
        )
    });
    with_store(application, |store| {
        store.record_provider_limit(provider, limit, plan)
    });
}

async fn refresh_limits(application: &AppHandle) {
    for provider in PROVIDERS {
        let providers = application.state::<ManagedProviders>();
        if providers.phase(provider) == Some(ProviderPhase::SignedIn) {
            read_limit(application, provider).await;
        }
    }
    publish(application, None);
}

/// Agents start somewhere real; the app data folder always exists and holds no project.
fn working_directory(application: &AppHandle) -> PathBuf {
    application
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
}

fn with_store(
    application: &AppHandle,
    change: impl FnOnce(&mut StateStore) -> Result<bool, velocity_store::StoreError>,
) {
    let state = application.state::<ManagedStateStore>();
    let Ok(mut store) = state.lock() else {
        return;
    };
    if let Err(error) = change(&mut store) {
        eprintln!("Could not save provider state: {error}");
    }
}

/// Publishes Settings → Providers and redraws the footer meter and the home model pill,
/// with an optional toast.
fn publish(application: &AppHandle, toast: Option<String>) -> Option<ProvidersView> {
    let runtime = application.state::<ManagedProviders>().runtime();
    let store_state = application.state::<ManagedStateStore>();
    let store = store_state.lock().ok()?;
    let view = store.providers_view(&ProviderContext::new(&runtime, unix_now()));
    let settings = store.settings();
    let layout_state = application.state::<MainLayout>();
    if let Ok(mut layout) = layout_state.lock() {
        let mut changed = layout.set_provider_summary(
            velocity_store::provider_views::provider_usage(&settings),
            velocity_store::provider_views::default_model_pill(&settings),
        );
        if let Some(message) = toast {
            layout.show_toast(message, None);
            changed = true;
        }
        if changed {
            let _ = crate::layout::publish(application, &layout.snapshot(), None);
        }
    }
    drop(store);
    if let Err(error) = (ProvidersChanged { view: view.clone() }).emit(application) {
        eprintln!("Could not publish providers: {error}");
    }
    Some(view)
}

fn current_view(application: &AppHandle) -> Result<ProvidersView, String> {
    publish(application, None).ok_or_else(|| "Could not read provider state".into())
}

#[tauri::command]
#[specta::specta]
pub fn list_providers(application: AppHandle) -> Result<ProvidersView, String> {
    current_view(&application)
}

/// Sign in with Claude / ChatGPT / Google / Kimi (F15): runs the agent's own login, which
/// opens the provider's OAuth page in your browser. The card waits until it finishes.
#[tauri::command]
#[specta::specta]
pub fn provider_sign_in(
    application: AppHandle,
    provider: ProviderKind,
) -> Result<ProvidersView, String> {
    let providers = application.state::<ManagedProviders>();
    // Signed out only in Velocity: the agent's login still holds, so no browser is needed.
    if providers.phase(provider) == Some(ProviderPhase::SignedIn) {
        with_store(&application, |store| {
            store.set_provider_signed_out(provider, false)
        });
        let message = signed_in_toast(&application, provider);
        return publish(&application, Some(message)).ok_or_else(|| "Could not sign in".into());
    }
    let method = providers
        .with_slot(provider, |slot| slot.method.clone())
        .flatten()
        .ok_or_else(|| {
            format!(
                "{}'s agent offers no sign-in Velocity can use. Sign in with its own CLI, then reopen Providers.",
                provider_facts(provider).name
            )
        })?;
    let handle = application.clone();
    let task = tauri::async_runtime::spawn(async move {
        finish_sign_in(&handle, provider, method).await;
    });
    providers.with_slot(provider, |slot| {
        if let Some(previous) = slot.sign_in.replace(task) {
            previous.abort();
        }
        slot.runtime.phase = ProviderPhase::Waiting;
        slot.runtime.detail = None;
    });
    let site = provider_facts(provider).site;
    let toast = format!("Opened {site} in your browser · finish signing in there");
    publish(&application, Some(toast)).ok_or_else(|| "Could not sign in".into())
}

async fn finish_sign_in(application: &AppHandle, provider: ProviderKind, method: SignInMethod) {
    let (launch, _) = agent_launch(provider);
    let directory = working_directory(application);
    let result = sign_in(&launch, &directory, &method, SIGN_IN_DEADLINE).await;
    let providers = application.state::<ManagedProviders>();
    providers.with_slot(provider, |slot| slot.sign_in = None);
    if let Err(error) = result {
        providers.with_slot(provider, |slot| {
            slot.runtime.phase = ProviderPhase::SignedOut;
            slot.runtime.detail = Some(error.to_string());
        });
        let name = provider_facts(provider).name;
        publish(
            application,
            Some(format!("{name} sign-in didn't finish: {error}")),
        );
        return;
    }
    with_store(application, |store| {
        store.set_provider_signed_out(provider, false)
    });
    check(application, provider).await;
    let message = signed_in_toast(application, provider);
    publish(application, Some(message));
}

/// `Anthropic signed in · Opus 5.5, Sonnet 5.5 available`.
fn signed_in_toast(application: &AppHandle, provider: ProviderKind) -> String {
    let name = provider_facts(provider).name;
    let store_state = application.state::<ManagedStateStore>();
    let models = store_state
        .lock()
        .map(|store| {
            store
                .settings()
                .visible_models(provider)
                .iter()
                .map(|model| model.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if models.is_empty() {
        format!("{name} signed in")
    } else {
        format!("{name} signed in · {} available", models.join(", "))
    }
}

/// Sign out (F15), or Cancel while waiting for the browser. Agents already running on the
/// provider keep their session until they finish; new work can't use it.
#[tauri::command]
#[specta::specta]
pub fn provider_sign_out(
    application: AppHandle,
    provider: ProviderKind,
) -> Result<ProvidersView, String> {
    let providers = application.state::<ManagedProviders>();
    let cancelled = providers
        .with_slot(provider, |slot| {
            let task = slot.sign_in.take();
            if let Some(task) = &task {
                task.abort();
                slot.runtime.phase = ProviderPhase::SignedOut;
            }
            task.is_some()
        })
        .unwrap_or(false);
    let name = provider_facts(provider).name;
    if cancelled {
        return publish(
            &application,
            Some(format!("Cancelled signing in to {name}")),
        )
        .ok_or_else(|| "Could not cancel".into());
    }
    with_store(&application, |store| {
        store.set_provider_signed_out(provider, true)
    });
    let running = running_agents(&application, provider);
    let toast = match running {
        0 => format!("Signed out of {name}"),
        1 => {
            format!("Signed out of {name} · its running agent keeps its session until it finishes")
        }
        count => format!(
            "Signed out of {name} · its {count} running agents keep their session until they finish"
        ),
    };
    publish(&application, Some(toast)).ok_or_else(|| "Could not sign out".into())
}

fn running_agents(application: &AppHandle, provider: ProviderKind) -> usize {
    let store_state = application.state::<ManagedStateStore>();
    let Ok(store) = store_state.lock() else {
        return 0;
    };
    store
        .state()
        .projects
        .iter()
        .flat_map(|project| project.threads.iter())
        .filter(|thread| {
            thread.provider == provider && thread.state == velocity_store::ThreadState::Working
        })
        .count()
}

/// Model chips, the default for new threads and the thread title model (F15).
#[tauri::command]
#[specta::specta]
pub fn set_provider_settings(
    application: AppHandle,
    edit: ProviderEdit,
) -> Result<ProvidersView, String> {
    {
        let store_state = application.state::<ManagedStateStore>();
        let mut store = store_state.lock().map_err(|error| error.to_string())?;
        store
            .edit_providers(edit)
            .map_err(|error| error.to_string())?;
    }
    current_view(&application)
}

/// The composer's model menu (F4): for a thread, or the defaults from the home composer.
#[tauri::command]
#[specta::specta]
pub fn get_model_menu(
    application: AppHandle,
    project_identifier: u32,
    thread_identifier: Option<u32>,
) -> Result<ModelMenuView, String> {
    model_menu(&application, project_identifier, thread_identifier)
}

fn model_menu(
    application: &AppHandle,
    project: u32,
    thread: Option<u32>,
) -> Result<ModelMenuView, String> {
    let live = match thread {
        Some(thread) => lock_threads(application)?.is_live((project, thread)),
        None => false,
    };
    let runtime = application.state::<ManagedProviders>().runtime();
    let store_state = application.state::<ManagedStateStore>();
    let store = store_state.lock().map_err(|error| error.to_string())?;
    let item = thread.and_then(|thread| {
        store
            .project(project)?
            .threads
            .iter()
            .find(|item| item.id == thread)
    });
    let live_provider = item.filter(|_| live).map(|item| item.provider);
    let target = MenuTarget::new(item, live_provider);
    Ok(store.model_menu(&target, &ProviderContext::new(&runtime, unix_now())))
}

/// A pick from the model menu. On a running thread it applies from the next message; the
/// thread stays on its provider's agent (F4).
#[tauri::command]
#[specta::specta]
pub fn choose_model(
    application: AppHandle,
    project_identifier: u32,
    thread_identifier: Option<u32>,
    choice: ModelMenuChoice,
) -> Result<ModelMenuView, String> {
    match thread_identifier {
        Some(thread) => choose_for_thread(&application, (project_identifier, thread), choice)?,
        None => {
            let edit = match choice {
                ModelMenuChoice::Model { provider, model } => {
                    ProviderEdit::DefaultModel { provider, model }
                }
                ModelMenuChoice::Effort(effort) => ProviderEdit::DefaultEffort(effort),
                ModelMenuChoice::Speed(speed) => ProviderEdit::DefaultSpeed(speed),
            };
            set_provider_settings(application.clone(), edit)?;
        }
    }
    model_menu(&application, project_identifier, thread_identifier)
}

fn choose_for_thread(
    application: &AppHandle,
    key: (u32, u32),
    choice: ModelMenuChoice,
) -> Result<(), String> {
    let live = lock_threads(application)?.is_live(key);
    let settings = {
        let store_state = application.state::<ManagedStateStore>();
        let mut store = store_state.lock().map_err(|error| error.to_string())?;
        let thread = store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|item| item.id == key.1))
            .ok_or("This thread no longer exists")?;
        if let ModelMenuChoice::Model { provider, model } = &choice
            && live
            && *provider != thread.provider
        {
            return Err(format!(
                "A running thread stays on {}. Start a new thread to use {model}.",
                provider_facts(thread.provider).name
            ));
        }
        store
            .choose_thread_model(key.0, key.1, choice)
            .map_err(|error| error.to_string())?;
        store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|item| item.id == key.1))
            .map(thread_run_settings)
    };
    if live && let Some(settings) = settings {
        // A session that ended meanwhile picks the settings up when it next starts.
        let _ = lock_threads(application)?.reconfigure(key, settings);
    }
    crate::threads::refresh(application, key, false).map(|_| ())
}

fn lock_threads(
    application: &AppHandle,
) -> Result<std::sync::MutexGuard<'_, ThreadRuntimes>, String> {
    application
        .state::<ManagedThreads>()
        .inner()
        .lock()
        .map_err(|error| error.to_string())
}
