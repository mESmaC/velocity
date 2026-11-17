//! Kimi Code's managed subscription report, using the selected CLI profile.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, redirect::Policy};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{AgentLaunch, ClientError, probe_agent};
use crate::usage_limits::{LimitReading, kimi_usage_reading};

const MAINLAND_API: &str = "https://api.kimi.com/coding/v1";
const GLOBAL_API: &str = "https://api.kimi.ai/coding/v1";
const MAINLAND_AUTH: &str = "https://auth.kimi.com";
const GLOBAL_AUTH: &str = "https://auth.kimi.ai";

struct CredentialSource {
    path: PathBuf,
    base: &'static str,
}

pub async fn read_kimi_account(
    launch: &AgentLaunch,
    directory: &Path,
    deadline: Duration,
) -> Result<(Option<LimitReading>, Option<String>), ClientError> {
    tokio::time::timeout(deadline, async {
        let source = credential_source(launch, directory).await?;
        let mut credentials = read_credentials(&source).await?;
        if token_expiring(&credentials) {
            // Only Kimi can renew its login safely. Its refresh tokens are single-use.
            probe_agent(launch, directory, deadline).await?;
            credentials = read_credentials(&source).await?;
        }
        let token = credentials
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(usage_error)?;
        let base = source.base;
        let client = Client::builder()
            .redirect(Policy::none())
            .build()
            .map_err(|_| usage_error())?;
        let (usage, identity) = tokio::join!(
            report(&client, format!("{base}/usages"), token),
            report(&client, format!("{base}/me"), token),
        );
        let label = identity.and_then(|account| {
            ["email", "nickname", "username"]
                .into_iter()
                .filter_map(|field| account.get(field).and_then(Value::as_str))
                .find(|label| !label.is_empty())
                .map(str::to_owned)
        });
        Ok((usage.as_ref().and_then(kimi_usage_reading), label))
    })
    .await
    .map_err(|_| ClientError::TimedOut(deadline))?
}

async fn credential_source(
    launch: &AgentLaunch,
    directory: &Path,
) -> Result<CredentialSource, ClientError> {
    let modern_override = profile_override(launch, directory, "KIMI_CODE_HOME");
    let legacy_override = profile_override(launch, directory, "KIMI_SHARE_DIR");
    let home = std::env::home_dir();
    let modern = match (&modern_override, &legacy_override) {
        (Some(profile), _) => Some(profile.clone()),
        (None, None) => home.as_ref().map(|path| path.join(".kimi-code")),
        (None, Some(_)) => None,
    };
    if let Some(profile) = modern {
        let config = profile_config(&profile).await?;
        let provider = managed_provider(&config);
        let source = modern_source(launch, &profile, provider)?;
        let exists = tokio::fs::try_exists(&source.path)
            .await
            .map_err(|_| usage_error())?;
        if provider.is_some() || exists {
            return Ok(source);
        }
    }

    // A configured profile must never read the default login of a different account.
    if modern_override.is_some() && legacy_override.is_none() {
        return Err(usage_error());
    }
    let legacy = legacy_override
        .or_else(|| home.map(|path| path.join(".kimi")))
        .ok_or_else(usage_error)?;
    let config = profile_config(&legacy).await?;
    let override_url = environment(launch, "KIMI_CODE_BASE_URL");
    let base = official_base(
        override_url
            .as_deref()
            .or_else(|| kimi_config_value(managed_provider(&config), "base_url")),
    )?;
    Ok(CredentialSource {
        path: legacy.join("credentials/kimi-code.json"),
        base,
    })
}

fn profile_override(launch: &AgentLaunch, directory: &Path, name: &str) -> Option<PathBuf> {
    let path = launch
        .env_value(name)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(name).map(PathBuf::from))
        .filter(|path| !path.as_os_str().is_empty())?;
    Some(directory.join(path))
}

async fn profile_config(profile: &Path) -> Result<toml::Value, ClientError> {
    match tokio::fs::read_to_string(profile.join("config.toml")).await {
        Ok(text) => toml::from_str(&text).map_err(|_| usage_error()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(toml::Value::Table(toml::Table::new()))
        }
        Err(_) => Err(usage_error()),
    }
}

fn managed_provider(config: &toml::Value) -> Option<&toml::Value> {
    config.get("providers")?.get("managed:kimi-code")
}

fn environment(launch: &AgentLaunch, name: &str) -> Option<String> {
    launch
        .env_value(name)
        .map(str::to_owned)
        .or_else(|| std::env::var(name).ok())
}

fn official_base(configured: Option<&str>) -> Result<&'static str, ClientError> {
    match configured.unwrap_or(MAINLAND_API).trim_end_matches('/') {
        MAINLAND_API => Ok(MAINLAND_API),
        GLOBAL_API => Ok(GLOBAL_API),
        _ => Err(usage_error()),
    }
}

fn kimi_config_value<'config>(
    config: Option<&'config toml::Value>,
    key: &str,
) -> Option<&'config str> {
    config?.get(key)?.as_str()
}

fn modern_source(
    launch: &AgentLaunch,
    profile: &Path,
    provider: Option<&toml::Value>,
) -> Result<CredentialSource, ClientError> {
    let oauth = provider.and_then(|provider| provider.get("oauth"));
    if kimi_config_value(oauth, "storage").is_some_and(|storage| storage != "file") {
        return Err(usage_error());
    }
    let override_url = environment(launch, "KIMI_CODE_BASE_URL");
    let override_auth = environment(launch, "KIMI_CODE_OAUTH_HOST")
        .or_else(|| environment(launch, "KIMI_OAUTH_HOST"));
    let base = official_base(
        override_url
            .as_deref()
            .or_else(|| kimi_config_value(provider, "base_url")),
    )?;
    let configured_auth = if override_url.is_some() || override_auth.is_some() {
        override_auth.as_deref()
    } else {
        kimi_config_value(oauth, "oauth_host")
    };
    let auth = configured_auth
        .unwrap_or(MAINLAND_AUTH)
        .trim_end_matches('/');
    if !matches!(auth, MAINLAND_AUTH | GLOBAL_AUTH) {
        return Err(usage_error());
    }
    let slot = if base == MAINLAND_API && auth == MAINLAND_AUTH {
        "kimi-code.json".into()
    } else {
        // Kimi hashes the ordered JSON pair to keep regional OAuth logins in distinct slots.
        let seed = format!(r#"{{"oauthHost":"{auth}","baseUrl":"{base}"}}"#);
        let digest = format!("{:x}", Sha256::digest(seed));
        format!("kimi-code-env-{}.json", &digest[..16])
    };
    Ok(CredentialSource {
        path: profile.join("credentials").join(slot),
        base,
    })
}

async fn read_credentials(source: &CredentialSource) -> Result<Value, ClientError> {
    let data = tokio::fs::read(&source.path)
        .await
        .map_err(|_| usage_error())?;
    serde_json::from_slice(&data).map_err(|_| usage_error())
}

fn token_expiring(credentials: &Value) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    credentials
        .get("expires_at")
        .and_then(Value::as_f64)
        .is_some_and(|expiry| expiry <= now + 30.0)
}

async fn report(client: &Client, endpoint: String, token: &str) -> Option<Value> {
    client
        .get(endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .bearer_auth(token)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()
}

fn usage_error() -> ClientError {
    ClientError::Usage("could not read the Kimi Code subscription".into())
}
