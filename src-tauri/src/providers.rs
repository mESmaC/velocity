//! Providers (SPEC.md F15): the live sign-in state of each provider's agent, sign-in and
//! sign-out, subscription limits, Settings → Providers and the composer's model menu.
//!
//! Each agent is asked at startup, after every sign-in and whenever its command changes;
//! missing agents are checked again whenever Settings → Providers opens.
//! limits are read every five minutes. What the agents report is saved in the store's provider catalog, so pickers and
//! the footer meter are right at the next start before the agents answer again.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_acp::client::AgentLaunch;
use velocity_acp::client::{
    AgentProbe, ClientError, probe_agent, read_antigravity_usage, read_claude_usage,
    read_codex_usage, sign_in,
};
use velocity_acp::{
    LimitReading, SignInMethod, agent_command_label, agent_command_line, choose_sign_in,
};
use velocity_store::provider_views::{
    MenuTarget, ModelMenuChoice, ModelMenuView, ProviderContext, ProviderPhase, ProviderRuntime,
    ProvidersView,
};
use velocity_store::providers::{ProviderEdit, ProviderLimit, ProviderModel, unix_now};
use velocity_store::{ProviderKind, Settings, StateStore};
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
    /// Bumped whenever the command changes, so a check of the old command can't record over
    /// what the new one reports.
    generation: u64,
    /// Prevent an older periodic read from replacing a report received during that read.
    limit_revision: u64,
}

/// Every provider's slot, in card order, kept in step with Settings → Providers.
#[derive(Default)]
pub struct ManagedProviders(Mutex<Vec<Slot>>);

impl ManagedProviders {
    /// Matches the slots to `settings`: a slot for every provider, built-in or added, with
    /// its current command. A slot whose command changed goes back to Checking. Returns the
    /// providers whose agent must be asked again.
    fn sync(&self, settings: &Settings) -> Vec<ProviderKind> {
        let Ok(mut slots) = self.0.lock() else {
            return Vec::new();
        };
        let mut previous: Vec<Slot> = std::mem::take(&mut *slots);
        let mut changed = Vec::new();
        for provider in settings.all_providers() {
            let launch = agent_launch(settings, provider)
                .ok()
                .map(|(launch, _)| launch);
            let line = launch.as_ref().map(agent_command_line).unwrap_or_default();
            let slot = match previous
                .iter()
                .position(|slot| slot.runtime.provider == provider)
            {
                Some(index) => previous.swap_remove(index),
                None => new_slot(provider),
            };
            slots.push(refresh_slot(slot, launch.as_ref(), line, &mut changed));
        }
        // Removed agents: stop a sign-in still waiting on the browser.
        for slot in previous {
            if let Some(task) = slot.sign_in {
                task.abort();
            }
        }
        changed
    }

    fn generation(&self, provider: ProviderKind) -> Option<u64> {
        self.with_slot(provider, |slot| slot.generation)
    }

    /// Runs `change` on the slot only while it still has `generation`'s command.
    fn with_current_slot(
        &self,
        provider: ProviderKind,
        generation: u64,
        change: impl FnOnce(&mut Slot),
    ) -> bool {
        self.with_slot(provider, |slot| {
            if slot.generation == generation {
                change(slot);
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
    }

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

    fn retry_missing(&self) -> Vec<ProviderKind> {
        let Ok(mut slots) = self.0.lock() else {
            return Vec::new();
        };
        let mut providers = Vec::new();
        for slot in slots.iter_mut() {
            if slot.runtime.phase == ProviderPhase::NotInstalled {
                // Reserve the probe before spawning: repeated opens must not start
                // competing agent processes for the same command.
                slot.runtime.phase = ProviderPhase::Checking;
                providers.push(slot.runtime.provider);
            }
        }
        providers
    }
}

fn new_slot(provider: ProviderKind) -> Slot {
    Slot {
        runtime: ProviderRuntime::new(provider, String::new(), String::new()),
        method: None,
        sign_in: None,
        generation: 0,
        limit_revision: 0,
    }
}

fn refresh_slot(
    mut slot: Slot,
    launch: Option<&AgentLaunch>,
    line: String,
    changed: &mut Vec<ProviderKind>,
) -> Slot {
    if slot.runtime.command_line != line || slot.generation == 0 {
        slot.generation += 1;
        slot.runtime.command = launch.map(agent_command_label).unwrap_or_default();
        slot.runtime.command_line = line;
        slot.runtime.phase = ProviderPhase::Checking;
        slot.runtime.can_sign_in = false;
        slot.runtime.detail = None;
        slot.method = None;
        if let Some(task) = slot.sign_in.take() {
            task.abort();
        }
        changed.push(slot.runtime.provider);
    }
    slot
}

/// Brings the slots in line with saved settings and asks every agent whose command changed.
pub(crate) fn sync_and_check(application: &AppHandle) {
    let settings = settings(application);
    let changed = application.state::<ManagedProviders>().sync(&settings);
    for provider in changed {
        let handle = application.clone();
        tauri::async_runtime::spawn(async move { check(&handle, provider).await });
    }
}

fn settings(application: &AppHandle) -> Settings {
    application
        .state::<ManagedStateStore>()
        .lock()
        .map(|store| store.settings())
        .unwrap_or_default()
}

fn configure_concurrency(application: &AppHandle) {
    let state = application.state::<ManagedStateStore>();
    let Ok(store) = state.lock() else {
        return;
    };
    // Keep settings edits serialized through applying the live cap as well as saving it.
    let limits = store
        .settings()
        .providers
        .concurrency
        .iter()
        .map(|entry| {
            (
                velocity_threads::titles::provider(entry.provider),
                entry.maximum,
            )
        })
        .collect::<Vec<_>>();
    velocity_acp::concurrency::configure(&limits);
    drop(store);
}

pub fn initialize(application: &AppHandle) {
    configure_concurrency(application);
    watch_live_limits(application);
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        let settings = settings(&handle);
        let changed = handle.state::<ManagedProviders>().sync(&settings);
        let checks: Vec<_> = changed
            .into_iter()
            .map(|provider| {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move { check(&handle, provider).await })
            })
            .collect();
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
    let providers = application.state::<ManagedProviders>();
    let Some(generation) = providers.generation(provider) else {
        return;
    };
    let Ok((launch, _)) = agent_launch(&settings(application), provider) else {
        return;
    };
    let result = probe_agent(&launch, &working_directory(application), PROBE_DEADLINE).await;
    let probe = match result {
        Ok(probe) => probe,
        Err(error) => {
            record_failure(application, (provider, generation), &error);
            publish(application, None);
            return;
        }
    };
    if !record_probe(application, (provider, generation), &probe) {
        return;
    }
    if probe.signed_in {
        read_limit(application, provider).await;
    }
    publish(application, None);
}

/// Records what the agent reported, unless its command changed meanwhile. Returns whether
/// it was recorded.
fn record_probe(
    application: &AppHandle,
    (provider, generation): (ProviderKind, u64),
    probe: &AgentProbe,
) -> bool {
    let method = choose_sign_in(&probe.sign_in_methods)
        .and_then(|index| probe.sign_in_methods.get(index))
        .cloned();
    let current =
        application
            .state::<ManagedProviders>()
            .with_current_slot(provider, generation, |slot| {
                slot.runtime.phase = if probe.signed_in {
                    ProviderPhase::SignedIn
                } else {
                    ProviderPhase::SignedOut
                };
                slot.runtime.can_sign_in = method.is_some();
                slot.runtime.detail = None;
                slot.method = method;
            });
    if !current {
        return false;
    }
    let models = probe
        .models
        .iter()
        .map(|model| ProviderModel::new(model.id.clone(), model.name.clone(), model.fast))
        .collect();
    with_store(application, |store| {
        store.record_provider_probe(provider, probe.signed_in, models)
    });
    true
}

fn record_failure(
    application: &AppHandle,
    (provider, generation): (ProviderKind, u64),
    error: &ClientError,
) {
    let missing = matches!(
        error,
        ClientError::Spawn { source, .. } if source.kind() == std::io::ErrorKind::NotFound
    );
    let current =
        application
            .state::<ManagedProviders>()
            .with_current_slot(provider, generation, |slot| {
                slot.runtime.phase = if missing {
                    ProviderPhase::NotInstalled
                } else {
                    ProviderPhase::Failed
                };
                slot.runtime.detail = (!missing).then(|| error.to_string());
            });
    if !current {
        return;
    }
    // An agent that can't start can't run work: its models leave the pickers.
    with_store(application, |store| {
        store.record_provider_probe(provider, false, Vec::new())
    });
}

/// Reads one provider's limit where it has a source (F15 Limit sources). A failed read shows
/// "limit not reported".
async fn read_limit(application: &AppHandle, provider: ProviderKind) {
    let providers = application.state::<ManagedProviders>();
    let revision = providers.with_slot(provider, |slot| (slot.generation, slot.limit_revision));
    let directory = working_directory(application);
    let reading = match provider {
        ProviderKind::Claude => read_claude_usage(&directory, LIMIT_DEADLINE).await,
        ProviderKind::Codex => read_codex_usage(&directory, LIMIT_DEADLINE).await,
        ProviderKind::Antigravity => {
            let launch = agent_launch(&settings(application), provider);
            match launch {
                Ok((launch, _)) => {
                    read_antigravity_usage(&launch, &directory, LIMIT_DEADLINE).await
                }
                Err(error) => Err(ClientError::Usage(error.to_string())),
            }
        }
        ProviderKind::Kimi | ProviderKind::Custom(_) => Ok(None),
    };
    let reading: Option<LimitReading> = reading.ok().flatten();
    providers.with_slot(provider, |slot| {
        if revision == Some((slot.generation, slot.limit_revision)) {
            record_limit(application, provider, reading);
        }
    });
}

fn record_limit(application: &AppHandle, provider: ProviderKind, reading: Option<LimitReading>) {
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

fn watch_live_limits(application: &AppHandle) {
    let mut reports = velocity_acp::client::subscribe_limits();
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match reports.recv().await {
                Ok((provider, reading)) => {
                    let provider = match provider {
                        velocity_acp::Provider::Anthropic => ProviderKind::Claude,
                        velocity_acp::Provider::Codex => ProviderKind::Codex,
                        _ => continue,
                    };
                    handle
                        .state::<ManagedProviders>()
                        .with_slot(provider, |slot| {
                            slot.limit_revision = slot.limit_revision.wrapping_add(1);
                            record_limit(&handle, provider, reading);
                        });
                    publish(&handle, None);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

async fn refresh_limits(application: &AppHandle) {
    for provider in settings(application).all_providers() {
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

pub(crate) fn current_view(application: &AppHandle) -> Result<ProvidersView, String> {
    publish(application, None).ok_or_else(|| "Could not read provider state".into())
}

#[tauri::command]
#[specta::specta]
pub fn list_providers(application: AppHandle) -> Result<ProvidersView, String> {
    let missing = application.state::<ManagedProviders>().retry_missing();
    let view = current_view(&application);
    for provider in missing {
        let handle = application.clone();
        tauri::async_runtime::spawn(async move { check(&handle, provider).await });
    }
    view
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
                settings(&application).provider_name(provider)
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
    let site = settings(&application).provider_facts(provider).site;
    let toast = format!("Opened {site} in your browser · finish signing in there");
    publish(&application, Some(toast)).ok_or_else(|| "Could not sign in".into())
}

async fn finish_sign_in(application: &AppHandle, provider: ProviderKind, method: SignInMethod) {
    let settings = settings(application);
    let Ok((launch, _)) = agent_launch(&settings, provider) else {
        return;
    };
    let directory = working_directory(application);
    let result = sign_in(&launch, &directory, &method, SIGN_IN_DEADLINE).await;
    let providers = application.state::<ManagedProviders>();
    providers.with_slot(provider, |slot| slot.sign_in = None);
    if let Err(error) = result {
        providers.with_slot(provider, |slot| {
            slot.runtime.phase = ProviderPhase::SignedOut;
            slot.runtime.detail = Some(error.to_string());
        });
        let name = settings.provider_name(provider);
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
    let name = settings(application).provider_name(provider);
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
    let name = settings(&application).provider_name(provider);
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
    configure_concurrency(&application);
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
                store.settings().provider_name(thread.provider)
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
