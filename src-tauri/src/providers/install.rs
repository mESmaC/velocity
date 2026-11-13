//! Install on a provider's card (F15 Install, #221): puts a built-in provider's ACP agent on
//! your `PATH` from Settings → Providers, then asks it again as a fresh check would.

use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_acp::client::{install_agent, program_found};
use velocity_acp::{InstallPlatform, InstallStep, install_steps};
use velocity_store::provider_views::{ProviderPhase, ProvidersView};
use velocity_store::{ProviderKind, Settings};

use super::{
    ManagedProviders, check, current_view, publish, settings, signed_in_toast, working_directory,
};

/// npm and winget downloads on a slow connection, plus Node.js first when npm is missing.
const INSTALL_DEADLINE: Duration = Duration::from_secs(20 * 60);

/// What Install runs for `provider`, in order. Only a built-in provider on its built-in
/// command: a command you changed runs a program Velocity can't know how to install.
fn install_plan(settings: &Settings, provider: ProviderKind) -> Option<Vec<InstallStep>> {
    if settings.agent_command(provider).is_some() {
        return None;
    }
    install_steps(
        velocity_threads::titles::provider(provider),
        InstallPlatform::current(),
        program_found("npm"),
    )
}

/// The plan as one line for the card's tooltip and the toast.
pub(super) fn install_command(settings: &Settings, provider: ProviderKind) -> Option<String> {
    install_plan(settings, provider).map(|steps| {
        steps
            .iter()
            .map(InstallStep::command_line)
            .collect::<Vec<_>>()
            .join(" && ")
    })
}

/// Install (F15): runs the provider's own package install with no console window. The card
/// shows Installing with Cancel until it finishes, then checks the agent again.
#[tauri::command]
#[specta::specta]
pub async fn install_provider(
    application: AppHandle,
    provider: ProviderKind,
) -> Result<ProvidersView, String> {
    crate::layout::off_ui_thread(move || {
        let settings = settings(&application);
        let name = settings.provider_name(provider);
        let steps = install_plan(&settings, provider).ok_or_else(|| {
            format!(
                "Velocity can't install {name}'s agent. Install it yourself, then reopen Providers."
            )
        })?;
        let providers = application.state::<ManagedProviders>();
        let command = steps
            .iter()
            .map(InstallStep::command_line)
            .collect::<Vec<_>>()
            .join(" && ");
        let handle = application.clone();
        // The task is started and recorded under the slot's lock. It clears `install` through
        // the same lock when it ends, so it can't end first and leave a finished task behind
        // that the next Sign out would take for an install to cancel.
        let started = providers
            .with_slot(provider, |slot| {
                if slot.runtime.phase != ProviderPhase::NotInstalled {
                    return false;
                }
                slot.runtime.phase = ProviderPhase::Installing;
                slot.runtime.detail = None;
                slot.runtime.install_failure = None;
                let generation = slot.generation;
                let task = tauri::async_runtime::spawn(async move {
                    finish_install(&handle, (provider, generation), steps).await;
                });
                if let Some(previous) = slot.install.replace(task) {
                    previous.abort();
                }
                true
            })
            .unwrap_or(false);
        // Already installing, or found meanwhile: the card shows where it stands.
        if !started {
            return current_view(&application);
        }
        let toast = format!("Installing {name}'s agent · {command}");
        publish(&application, Some(toast)).ok_or_else(|| "Could not install".into())
    })
    .await
}

async fn finish_install(
    application: &AppHandle,
    (provider, generation): (ProviderKind, u64),
    steps: Vec<InstallStep>,
) {
    let result = install_agent(&steps, &working_directory(application), INSTALL_DEADLINE).await;
    let providers = application.state::<ManagedProviders>();
    let name = settings(application).provider_name(provider);
    let current = providers.with_current_slot(provider, generation, |slot| {
        slot.install = None;
        match &result {
            Ok(()) => slot.runtime.phase = ProviderPhase::Checking,
            Err(error) => {
                slot.runtime.phase = ProviderPhase::NotInstalled;
                slot.runtime.install_failure = Some(error.to_string());
            }
        }
    });
    if !current {
        return;
    }
    if let Err(error) = result {
        publish(
            application,
            Some(format!("{name}'s agent didn't install: {error}")),
        );
        return;
    }
    publish(application, None);
    check(application, provider).await;
    let toast = match providers.with_slot(provider, |slot| slot.runtime.phase) {
        Some(ProviderPhase::SignedIn) => signed_in_toast(application, provider),
        Some(ProviderPhase::NotInstalled) => format!(
            "{name}'s agent installed, but Velocity can't find it on your PATH yet. Restart Velocity."
        ),
        _ => {
            let sign_in = settings(application).provider_facts(provider).sign_in_label;
            format!("{name}'s agent installed · {sign_in} to use it")
        }
    };
    publish(application, Some(toast));
}
