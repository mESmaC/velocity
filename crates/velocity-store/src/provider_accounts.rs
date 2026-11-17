//! F15 account metadata and routing. The CLI owns everything in its profile directory.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::providers::{ProviderLimit, ProviderModel};
use crate::{ProviderKind, Settings, StateStore, StoreError, ThreadItem};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProviderAccount {
    pub provider: ProviderKind,
    pub id: u32,
    #[serde(default)]
    pub directory: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub identity: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub limit: Option<ProviderLimit>,
    #[serde(default)]
    pub blocked_until: Option<u32>,
    #[serde(default)]
    pub signed_in: bool,
    #[serde(default)]
    pub models: Vec<ProviderModel>,
}

impl ProviderAccount {
    fn new(provider: ProviderKind, id: u32, directory: String) -> Self {
        Self {
            provider,
            id,
            directory,
            name: None,
            identity: None,
            plan: None,
            limit: None,
            blocked_until: None,
            signed_in: false,
            models: Vec::new(),
        }
    }

    pub fn label(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.identity.clone())
            .or_else(|| self.plan.clone())
            .unwrap_or_else(|| {
                if self.id == 0 {
                    "CLI account".into()
                } else {
                    format!("Account {}", self.id.saturating_add(1))
                }
            })
    }

    pub fn available_at(&self, now: u32) -> bool {
        self.signed_in
            && self.blocked_until.is_none_or(|until| until <= now)
            && self
                .limit
                .as_ref()
                .is_none_or(|limit| !limit.exhausted(now))
    }
}

impl Settings {
    pub(crate) fn legacy_account_signed_in(&self, provider: ProviderKind) -> bool {
        !self.providers.signed_out.contains(&provider)
            && self
                .provider_catalog(provider)
                .is_none_or(|entry| entry.signed_in)
    }

    pub fn provider_accounts(&self, provider: ProviderKind) -> Vec<ProviderAccount> {
        let mut accounts: Vec<_> = self
            .providers
            .accounts
            .iter()
            .filter(|account| account.provider == provider && account.id != 0)
            .cloned()
            .collect();
        let mut legacy = self
            .saved_account(provider, 0)
            .cloned()
            .unwrap_or_else(|| ProviderAccount::new(provider, 0, String::new()));
        let catalog = self.provider_catalog(provider);
        legacy.signed_in = self.legacy_account_signed_in(provider);
        legacy.plan = catalog.and_then(|entry| entry.plan.clone());
        legacy.limit = catalog.and_then(|entry| entry.limit.clone());
        accounts.insert(0, legacy);
        accounts
    }

    pub fn saved_account(&self, provider: ProviderKind, id: u32) -> Option<&ProviderAccount> {
        self.providers
            .accounts
            .iter()
            .find(|account| account.provider == provider && account.id == id)
    }

    pub fn default_account(&self, provider: ProviderKind) -> u32 {
        self.providers
            .default_accounts
            .get(&provider.key())
            .copied()
            .unwrap_or(0)
    }

    pub fn composer_account(&self, provider: ProviderKind) -> u32 {
        self.providers
            .composer_account
            .filter(|_| self.providers.default_provider == provider)
            .unwrap_or_else(|| self.default_account(provider))
    }

    /// Search in card order, starting at the requested account and wrapping within the provider.
    pub fn available_account(
        &self,
        provider: ProviderKind,
        preferred: u32,
        now: u32,
    ) -> Option<u32> {
        if !self.provider_exists(provider) {
            return None;
        }
        let accounts = self.provider_accounts(provider);
        let start = accounts
            .iter()
            .position(|account| account.id == preferred)
            .unwrap_or(0);
        accounts[start..]
            .iter()
            .chain(&accounts[..start])
            .find(|account| account.available_at(now))
            .map(|account| account.id)
    }

    pub fn launch_account(&self, provider: ProviderKind, preferred: u32, now: u32) -> u32 {
        self.available_account(provider, preferred, now)
            .unwrap_or_else(|| {
                let accounts = self.provider_accounts(provider);
                accounts
                    .iter()
                    .find(|account| account.id == preferred && account.signed_in)
                    .or_else(|| accounts.iter().find(|account| account.signed_in))
                    .map_or(preferred, |account| account.id)
            })
    }

    pub fn account_label(&self, provider: ProviderKind, id: u32) -> Option<String> {
        self.provider_accounts(provider)
            .into_iter()
            .find(|account| account.id == id)
            .map(|account| account.label())
    }
}

impl ThreadItem {
    pub fn bind_provider_account(&mut self, account: u32) {
        self.preferred_account = None;
        if self.provider_account != account {
            self.agent_session = None;
            self.provider_account = account;
        }
    }
}

impl StateStore {
    pub fn add_provider_account(
        &mut self,
        provider: ProviderKind,
        directory: &std::path::Path,
    ) -> Result<u32, StoreError> {
        let mut settings = self.settings();
        let id = settings
            .providers
            .accounts
            .iter()
            .map(|account| account.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        settings.providers.accounts.push(ProviderAccount::new(
            provider,
            id,
            directory
                .join(id.to_string())
                .to_string_lossy()
                .into_owned(),
        ));
        self.replace_settings(settings)?;
        Ok(id)
    }

    pub fn update_provider_account(
        &mut self,
        provider: ProviderKind,
        id: u32,
        change: impl FnOnce(&mut ProviderAccount),
    ) -> Result<bool, StoreError> {
        let mut settings = self.settings();
        let accounts = &mut settings.providers.accounts;
        if let Some(account) = accounts
            .iter_mut()
            .find(|entry| entry.provider == provider && entry.id == id)
        {
            change(account);
        } else if id == 0 {
            let mut account = ProviderAccount::new(provider, id, String::new());
            change(&mut account);
            accounts.push(account);
        }
        self.replace_settings(settings)
    }

    pub fn record_account_probe(
        &mut self,
        provider: ProviderKind,
        id: u32,
        signed_in: bool,
        models: Vec<ProviderModel>,
    ) -> Result<bool, StoreError> {
        let mut settings = self.settings();
        if let Some(account) = settings
            .providers
            .accounts
            .iter_mut()
            .find(|account| account.provider == provider && account.id == id)
        {
            account.signed_in = signed_in;
            if signed_in && !models.is_empty() {
                account.models = models;
            }
        }
        self.replace_settings(settings)
    }

    pub fn set_default_account(
        &mut self,
        provider: ProviderKind,
        id: u32,
    ) -> Result<bool, StoreError> {
        let mut settings = self.settings();
        settings
            .providers
            .default_accounts
            .insert(provider.key(), id);
        if settings.providers.default_provider == provider {
            settings.providers.composer_account = None;
        }
        self.replace_settings(settings)
    }

    pub fn choose_composer_account(
        &mut self,
        provider: ProviderKind,
        id: u32,
    ) -> Result<bool, StoreError> {
        let mut settings = self.settings();
        if settings.providers.default_provider != provider {
            settings.providers.default_model = Some(settings.provider_model(provider));
            settings.providers.default_provider = provider;
        }
        settings.providers.composer_account = Some(id);
        self.replace_settings(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A saved single login must keep its usage, and reopening the store must not
    // make a dispatch pick that account while its subscription is exhausted.
    #[test]
    fn existing_login_and_second_account_survive_reload() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory =
            std::env::temp_dir().join(format!("velocity-provider-accounts-{}", std::process::id()));
        let provider = ProviderKind::Claude;
        let mut store = StateStore::load_or_default(&directory)?;
        store.record_provider_probe(provider, true, Vec::new())?;
        store.record_provider_limit(
            provider,
            Some(ProviderLimit::new(100, "weekly".into(), Some(200))),
            Some("Max".into()),
        )?;
        let mut old = serde_json::to_value(store.state())?;
        let providers = old
            .pointer_mut("/settings/providers")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("Missing provider settings")?;
        for key in ["accounts", "default_accounts", "composer_account"] {
            providers.remove(key);
        }
        std::fs::write(
            directory.join(crate::STATE_FILE_NAME),
            serde_json::to_vec(&old)?,
        )?;
        let mut store = StateStore::load_or_default(&directory)?;
        let second = store.add_provider_account(provider, &directory.join("profiles"))?;
        store.update_provider_account(provider, second, |account| {
            account.signed_in = true;
            account.name = Some("Second subscription".into());
            account.limit = Some(ProviderLimit::new(27, "weekly".into(), Some(250)));
        })?;
        let saved = StateStore::load_or_default(&directory)?.settings();
        let accounts = saved.provider_accounts(provider);
        assert_eq!(
            accounts
                .iter()
                .map(ProviderAccount::label)
                .collect::<Vec<_>>(),
            ["Max", "Second subscription"]
        );
        let readings = crate::provider_views::provider_usage_at(&saved, 100);
        assert_eq!(
            readings
                .iter()
                .map(|entry| entry.percent_used)
                .collect::<Vec<_>>(),
            [100, 27]
        );
        assert_eq!(
            saved.available_account(provider, saved.default_account(provider), 100),
            Some(second)
        );
        std::fs::remove_file(directory.join(crate::STATE_FILE_NAME))?;
        std::fs::remove_dir(directory)?;
        Ok(())
    }
}
