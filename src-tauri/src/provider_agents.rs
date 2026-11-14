//! Settings → Providers commands for ACP agent commands (SPEC.md F15): edit a provider's
//! command or Reset it to the built-in one, and Add an ACP agent beside the four. Each change
//! is saved, then the agent is asked again with its new command.

use tauri::{AppHandle, Manager};
use velocity_store::ProviderKind;
use velocity_store::provider_views::ProvidersView;

use crate::providers::{current_view, sync_and_check};
use crate::state::ManagedStateStore;

fn change_providers<T>(
    application: &AppHandle,
    change: impl FnOnce(
        &mut velocity_store::StateStore,
    ) -> Result<T, velocity_store::providers::AgentCommandError>,
) -> Result<T, String> {
    let value = {
        let store_state = application.state::<ManagedStateStore>();
        let mut store = store_state.lock().map_err(|error| error.to_string())?;
        change(&mut store).map_err(|error| error.to_string())?
    };
    sync_and_check(application);
    Ok(value)
}

/// Sets `provider`'s ACP agent command from its card: the program and its arguments as one
/// line. `None` resets a built-in provider to its built-in command. Running agents keep the
/// command they started with; new work uses the new one.
#[tauri::command]
#[specta::specta]
pub async fn set_provider_command(
    application: AppHandle,
    provider: ProviderKind,
    command: Option<String>,
) -> Result<ProvidersView, String> {
    crate::layout::off_ui_thread(move || {
        change_providers(&application, |store| {
            store.set_provider_command(provider, command.as_deref())
        })?;
        current_view(&application)
    })
    .await
}

/// Add an ACP agent (F15 Custom ACP agents): a name, and the command with its arguments. It
/// gets its own card and is read over ACP like a built-in provider. There is no field for an
/// environment variable or a key; the agent inherits your environment.
#[tauri::command]
#[specta::specta]
pub async fn add_custom_agent(
    application: AppHandle,
    name: String,
    command: String,
) -> Result<ProvidersView, String> {
    crate::layout::off_ui_thread(move || {
        change_providers(&application, |store| {
            store.add_custom_agent(&name, &command)
        })?;
        current_view(&application)
    })
    .await
}

/// Removes an agent you added. Its threads keep their record; new work can't start on it.
#[tauri::command]
#[specta::specta]
pub async fn remove_custom_agent(
    application: AppHandle,
    provider: ProviderKind,
) -> Result<ProvidersView, String> {
    crate::layout::off_ui_thread(move || {
        let ProviderKind::Custom(identifier) = provider else {
            return Err("Only an agent you added can be removed".into());
        };
        change_providers(&application, |store| store.remove_custom_agent(identifier))?;
        current_view(&application)
    })
    .await
}
