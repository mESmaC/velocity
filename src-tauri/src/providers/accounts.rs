//! IPC and OAuth for each provider's own profile.

use super::*;
use serde::{Deserialize, Serialize};
use velocity_store::provider_accounts::ProviderAccount;

#[derive(Deserialize, Serialize, specta::Type)]
pub enum ProviderAccountAction {
    Add,
    SignIn { account: u32 },
    SignOut { account: u32 },
    Cancel,
    Rename { account: u32, name: String },
    Default { account: u32 },
}

#[tauri::command]
#[specta::specta]
pub async fn edit_provider_account(
    application: AppHandle,
    provider: ProviderKind,
    action: ProviderAccountAction,
) -> Result<ProvidersView, String> {
    crate::layout::off_ui_thread(move || edit(&application, provider, action)).await
}

fn edit(
    application: &AppHandle,
    provider: ProviderKind,
    action: ProviderAccountAction,
) -> Result<ProvidersView, String> {
    match action {
        ProviderAccountAction::Add => start_login(application, provider, None)?,
        ProviderAccountAction::SignIn { account } => {
            start_login(application, provider, Some(account))?
        }
        ProviderAccountAction::Cancel => {
            application
                .state::<ManagedProviders>()
                .with_slot(provider, |slot| {
                    if let Some(task) = slot.account_login.take() {
                        task.abort();
                    }
                    slot.runtime.account_waiting = None;
                    slot.runtime.account_error = None;
                });
        }
        change => save_edit(application, provider, change)?,
    }
    current_view(application)
}

fn save_edit(
    application: &AppHandle,
    provider: ProviderKind,
    action: ProviderAccountAction,
) -> Result<(), String> {
    let state = application.state::<ManagedStateStore>();
    let mut store = state.lock().map_err(|error| error.to_string())?;
    let account = match &action {
        ProviderAccountAction::SignOut { account }
        | ProviderAccountAction::Rename { account, .. }
        | ProviderAccountAction::Default { account } => *account,
        _ => return Err("Choose an account action".into()),
    };
    let profile = store
        .settings()
        .provider_accounts(provider)
        .into_iter()
        .find(|profile| profile.id == account)
        .ok_or("Account not found")?;
    let result = match action {
        ProviderAccountAction::SignOut { .. } if account == 0 => {
            store.set_provider_signed_out(provider, true)
        }
        ProviderAccountAction::SignOut { .. } => {
            store.update_provider_account(provider, account, |profile| profile.signed_in = false)
        }
        ProviderAccountAction::Rename { name, .. } => {
            store.update_provider_account(provider, account, |profile| {
                let name = name.trim();
                profile.name = (!name.is_empty()).then(|| name.to_owned());
            })
        }
        ProviderAccountAction::Default { .. } if profile.signed_in => {
            store.set_default_account(provider, account)
        }
        _ => return Err("Sign in to the account first".into()),
    };
    result.map_err(|error| error.to_string())?;
    drop(store);
    crate::threads::refresh_account_labels(application)?;
    Ok(())
}

fn start_login(
    application: &AppHandle,
    provider: ProviderKind,
    existing: Option<u32>,
) -> Result<(), String> {
    if matches!(provider, ProviderKind::Custom(_)) {
        return Err("This agent does not define an isolated login profile".into());
    }
    let managed = application.state::<ManagedProviders>();
    managed
        .with_slot(provider, |slot| {
            if slot.sign_in.is_some() || slot.account_login.is_some() || slot.install.is_some() {
                return Err("Wait for the current provider operation to finish".into());
            }
            let id = login_profile(application, provider, existing)?;
            let generation = slot.generation;
            slot.runtime.account_waiting = Some(id);
            slot.runtime.account_error = None;
            let handle = application.clone();
            slot.account_login = Some(tauri::async_runtime::spawn(async move {
                finish_login(&handle, provider, id, generation).await;
            }));
            Ok(())
        })
        .ok_or_else(|| "Provider not found".to_owned())?
}

fn login_profile(
    application: &AppHandle,
    provider: ProviderKind,
    existing: Option<u32>,
) -> Result<u32, String> {
    let state = application.state::<ManagedStateStore>();
    let mut store = state.lock().map_err(|error| error.to_string())?;
    if let Some(id) = existing {
        return store
            .settings()
            .saved_account(provider, id)
            .filter(|profile| id != 0 && !profile.signed_in)
            .map(|profile| profile.id)
            .ok_or_else(|| "Account cannot be signed in here".into());
    }
    let directory = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?
        .join("provider-profiles")
        .join(provider.key());
    store
        .add_provider_account(provider, &directory)
        .map_err(|error| error.to_string())
}

async fn finish_login(application: &AppHandle, provider: ProviderKind, id: u32, generation: u64) {
    let result = login(application, provider, id).await;
    let current =
        application
            .state::<ManagedProviders>()
            .with_current_slot(provider, generation, |slot| {
                if slot.runtime.account_waiting != Some(id) {
                    return;
                }
                slot.account_login = None;
                slot.runtime.account_waiting = None;
                match result {
                    Ok(probe) => save_probe(application, provider, id, probe),
                    Err(error) => slot.runtime.account_error = Some(error),
                }
            });
    if current {
        publish(application, None);
        if settings(application)
            .saved_account(provider, id)
            .is_some_and(|account| account.signed_in)
        {
            read_limit(application, provider, id).await;
            publish(application, None);
        }
    }
}

async fn login(
    application: &AppHandle,
    provider: ProviderKind,
    id: u32,
) -> Result<AgentProbe, String> {
    let settings = settings(application);
    let profile = settings
        .saved_account(provider, id)
        .ok_or("Account not found")?;
    prepare_profile(profile)
        .await
        .map_err(|error| error.to_string())?;
    let (launch, _) =
        agent_launch_for_account(&settings, provider, id).map_err(|error| error.to_string())?;
    let directory = working_directory(application);
    let before = probe_profile(&launch, &directory, provider)
        .await
        .map_err(|error| error.to_string())?;
    if before.signed_in {
        return Ok(before);
    }
    let method = choose_sign_in(&before.sign_in_methods)
        .and_then(|index| before.sign_in_methods.get(index))
        .ok_or("The agent offers no subscription sign-in")?;
    sign_in(&launch, &directory, method, SIGN_IN_DEADLINE)
        .await
        .map_err(|error| error.to_string())?;
    let probe = probe_profile(&launch, &directory, provider)
        .await
        .map_err(|error| error.to_string())?;
    if probe.signed_in {
        Ok(probe)
    } else {
        Err("The agent did not confirm sign-in".into())
    }
}

async fn probe_profile(
    launch: &AgentLaunch,
    directory: &std::path::Path,
    provider: ProviderKind,
) -> Result<AgentProbe, ClientError> {
    let mut probe = probe_agent(launch, directory, PROBE_DEADLINE).await?;
    if provider == ProviderKind::Antigravity {
        probe.signed_in &= velocity_acp::client::antigravity_profile_signed_in(launch).await;
    }
    Ok(probe)
}

async fn prepare_profile(profile: &ProviderAccount) -> std::io::Result<()> {
    tokio::fs::create_dir_all(&profile.directory).await?;
    if profile.provider == ProviderKind::Codex {
        use tokio::io::AsyncWriteExt;
        let config = PathBuf::from(&profile.directory).join("config.toml");
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(config)
            .await
        {
            Ok(mut config) => {
                config
                    .write_all(b"cli_auth_credentials_store = \"file\"\n")
                    .await?
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn save_probe(application: &AppHandle, provider: ProviderKind, id: u32, probe: AgentProbe) {
    let models = probe
        .models
        .into_iter()
        .map(|model| ProviderModel::new(model.id, model.name, model.fast))
        .collect();
    with_store(application, |store| {
        store.record_account_probe(provider, id, probe.signed_in, models)
    });
}

pub(super) async fn refresh_profiles(
    application: &AppHandle,
    provider: ProviderKind,
    generation: u64,
) {
    let settings = settings(application);
    for profile in settings
        .provider_accounts(provider)
        .into_iter()
        .filter(|profile| profile.id != 0 && profile.signed_in)
    {
        let Ok((launch, _)) = agent_launch_for_account(&settings, provider, profile.id) else {
            continue;
        };
        let directory = working_directory(application);
        if let Ok(probe) = probe_profile(&launch, &directory, provider).await {
            application
                .state::<ManagedProviders>()
                .with_current_slot(provider, generation, |_| {
                    if super::settings(application)
                        .saved_account(provider, profile.id)
                        .is_some_and(|account| account.signed_in)
                    {
                        save_probe(application, provider, profile.id, probe);
                    }
                });
        }
        read_limit(application, provider, profile.id).await;
    }
}
