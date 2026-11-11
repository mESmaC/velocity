//! F15: Google's report using agy's own login, never a Velocity credential cache.

use std::path::Path;
use std::time::{Duration, SystemTime};

use reqwest::{Client, redirect::Policy};
use serde_json::{Value, json};

use super::{AgentLaunch, ClientError, probe_agent};
use crate::LimitReading;
use crate::antigravity_limits::{antigravity_model_reading, antigravity_usage_reading};

const HOSTS: [&str; 2] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];

/// Uses the native CLI's Google login. The whole operation, including credential access
/// and agy's own refresh when needed, has one deadline.
pub async fn read_antigravity_usage(
    launch: &AgentLaunch,
    directory: &Path,
    deadline: Duration,
) -> Result<Option<LimitReading>, ClientError> {
    tokio::time::timeout(deadline, async {
        let mut credentials = native_credentials().await?;
        if native_token_expired(&credentials) {
            // Let agy refresh its own keyring token. No prompt or authenticate request is
            // sent, and no OAuth client identity is copied from another application.
            probe_agent(launch, directory, deadline).await?;
            credentials = native_credentials().await?;
        }
        read_usage(deadline, &credentials).await
    })
    .await
    .map_err(|_| ClientError::TimedOut(deadline))?
}

async fn read_usage(
    deadline: Duration,
    credentials: &Value,
) -> Result<Option<LimitReading>, ClientError> {
    let client = Client::builder()
        .timeout(deadline.min(Duration::from_secs(10)))
        .redirect(Policy::none())
        .build()
        .map_err(|_| usage_error("could not create the Google quota client"))?;
    let token = token_string(credentials.get("token").unwrap_or(credentials))?;
    let body = credentials
        .get("project_id")
        .and_then(Value::as_str)
        .map_or_else(|| json!({}), |project| json!({"project": project}));
    for (operation, parse) in [
        (
            "retrieveUserQuotaSummary",
            antigravity_usage_reading as fn(&Value) -> Option<LimitReading>,
        ),
        ("fetchAvailableModels", antigravity_model_reading),
    ] {
        let mut readings = Vec::new();
        for host in HOSTS {
            if let Ok(report) = report(&client, host, operation, &token, &body).await
                && let Some(reading) = parse(&report)
            {
                readings.push(reading);
            }
        }
        if let Some(reading) = readings
            .into_iter()
            .max_by_key(|reading| reading.percent_used)
        {
            return Ok(Some(reading));
        }
    }
    Ok(None)
}

async fn report(
    client: &Client,
    host: &str,
    operation: &str,
    token: &str,
    body: &Value,
) -> Result<Value, ClientError> {
    let response = client
        .post(format!("{host}/v1internal:{operation}"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(|_| usage_error("Google quota request failed"))?
        .error_for_status()
        .map_err(|_| usage_error("Google quota request was rejected"))?;
    response
        .json()
        .await
        .map_err(|_| usage_error("invalid Google quota report"))
}

fn native_token_expired(credentials: &Value) -> bool {
    let Some(token) = credentials.get("token") else {
        return false;
    };
    let expiry = token
        .get("expiry")
        .and_then(Value::as_str)
        .and_then(crate::usage_limits::unix_seconds);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    expiry.is_none_or(|expiry| expiry <= now + 60)
}

#[cfg(windows)]
async fn native_credentials() -> Result<Value, ClientError> {
    tokio::task::spawn_blocking(|| {
        // go-keyring's Windows target is service:user. Nothing is written back.
        let entry = keyring::Entry::new_with_target("gemini:antigravity", "gemini", "antigravity")
            .map_err(|_| usage_error("could not open agy's credential store"))?;
        let bytes = entry
            .get_secret()
            .map_err(|_| usage_error("agy has no readable Google login"))?;
        serde_json::from_slice(&bytes).map_err(|_| usage_error("invalid agy credential record"))
    })
    .await
    .map_err(|_| usage_error("could not read agy's credential store"))?
}

#[cfg(not(windows))]
async fn native_credentials() -> Result<Value, ClientError> {
    Err(usage_error("no agy ACP credential record"))
}

fn token_string(credentials: &Value) -> Result<String, ClientError> {
    credentials
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| usage_error("agy has no current Google access token"))
}

fn usage_error(message: &str) -> ClientError {
    // Never put a response body or credential record in an error or transcript.
    ClientError::Usage(message.into())
}
