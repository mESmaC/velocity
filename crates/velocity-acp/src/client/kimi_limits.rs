//! Kimi Code's managed subscription report, using the selected CLI profile.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, redirect::Policy};
use serde_json::Value;

use super::{AgentLaunch, ClientError, probe_agent};
use crate::usage_limits::{LimitReading, kimi_usage_reading};

pub async fn read_kimi_account(
    launch: &AgentLaunch,
    directory: &Path,
    deadline: Duration,
) -> Result<(Option<LimitReading>, Option<String>), ClientError> {
    tokio::time::timeout(deadline, async {
        let profile = profile_directory(launch).ok_or_else(usage_error)?;
        let mut credentials = read_credentials(&profile).await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if credentials
            .get("expires_at")
            .and_then(Value::as_u64)
            .is_some_and(|expiry| expiry <= now + 60)
        {
            probe_agent(launch, directory, deadline).await?;
            credentials = read_credentials(&profile).await?;
        }
        let token = credentials
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(usage_error)?;
        let base = subscription_base(&profile).await;
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

fn profile_directory(launch: &AgentLaunch) -> Option<PathBuf> {
    launch
        .env_value("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("KIMI_CODE_HOME").map(PathBuf::from))
        .or_else(|| {
            ["USERPROFILE", "HOME"]
                .into_iter()
                .find_map(std::env::var_os)
                .map(|root| PathBuf::from(root).join(".kimi-code"))
        })
}

async fn read_credentials(directory: &Path) -> Result<Value, ClientError> {
    let data = tokio::fs::read(directory.join("credentials/kimi-code.json"))
        .await
        .map_err(|_| usage_error())?;
    serde_json::from_slice(&data).map_err(|_| usage_error())
}

async fn subscription_base(directory: &Path) -> &'static str {
    const GLOBAL: &str = "https://api.kimi.ai/coding/v1";
    let config = tokio::fs::read_to_string(directory.join("config.toml"))
        .await
        .ok();
    let config = config.and_then(|text| toml::from_str::<toml::Value>(&text).ok());
    let endpoint = config.as_ref().and_then(|config| {
        config
            .get("providers")?
            .get("managed:kimi-code")?
            .get("base_url")?
            .as_str()
    });
    // A custom endpoint must never receive the subscription login.
    if endpoint.is_some_and(|url| url.trim_end_matches('/') == GLOBAL) {
        GLOBAL
    } else {
        "https://api.kimi.com/coding/v1"
    }
}

async fn report(client: &Client, endpoint: String, token: &str) -> Option<Value> {
    client
        .get(endpoint)
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
