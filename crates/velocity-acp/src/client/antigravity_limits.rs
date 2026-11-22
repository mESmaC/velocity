//! F15: Google's quota report using agy's own login.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, redirect::Policy};
use serde_json::{Value, json};

use super::{AgentLaunch, ClientError, probe_agent};
use crate::LimitReading;
use crate::antigravity_limits::{QuotaWindow, merged_reading, model_windows, summary_windows};
use crate::usage_limits::unix_seconds;

const DAILY: &str = "https://daily-cloudcode-pa.googleapis.com";
const PRODUCTION: &str = "https://cloudcode-pa.googleapis.com";
const SUMMARY: &str = "retrieveUserQuotaSummary";

/// Credentials stay in agy's store. The deadline includes its refresh and both quota probes.
pub async fn read_antigravity_usage(
    launch: &AgentLaunch,
    directory: &Path,
    deadline: Duration,
    log_failure: impl Fn(&str, &ClientError),
) -> Result<Option<LimitReading>, ClientError> {
    tokio::time::timeout(deadline, async {
        let mut credentials = account_credentials(launch).await?;
        if token_expired(&credentials) {
            // agy's startup refreshes its native login without a model turn or a sign-in prompt.
            probe_agent(launch, directory, deadline)
                .await
                .map_err(|error| match error {
                    ClientError::Protocol(_) => {
                        usage_error("agy could not refresh its Google login")
                    }
                    other => other,
                })?;
            credentials = account_credentials(launch).await?;
        }
        read_usage(&credentials, &log_failure).await.map(Some)
    })
    .await
    .map_err(|_| ClientError::TimedOut(deadline))?
}

async fn read_usage(
    credentials: &Value,
    log_failure: &impl Fn(&str, &ClientError),
) -> Result<LimitReading, ClientError> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(Policy::none())
        .user_agent(user_agent())
        .build()
        .map_err(|_| usage_error("could not create the Google quota client"))?;
    let token = access_token(credentials)?;
    let body = credentials
        .get("project_id")
        .and_then(Value::as_str)
        .filter(|project| !project.is_empty())
        .map_or_else(|| json!({}), |project| json!({"project": project}));
    // The CLI uses a personal Google login. T3 Code gives the daily host priority for
    // consumer quota because the production host can return untouched copies of its buckets.
    let mut failure = None;
    for (operation, parse) in [
        (SUMMARY, summary_windows as ParseWindows),
        ("fetchAvailableModels", model_windows),
    ] {
        let (daily, production) =
            quota_reports(&client, token, &body, operation, parse, log_failure).await;
        let daily = daily.unwrap_or_else(|error| {
            failure.get_or_insert(error);
            Vec::new()
        });
        if let Some(reading) = merged_reading(daily, production.unwrap_or_default()) {
            return Ok(reading);
        }
    }
    Err(failure.unwrap_or_else(|| usage_error("Google returned no usable quota")))
}

type ParseWindows = fn(&Value, i64) -> Result<Vec<QuotaWindow>, String>;
type QuotaReport = Result<Vec<QuotaWindow>, ClientError>;

async fn quota_reports(
    client: &Client,
    token: &str,
    body: &Value,
    operation: &str,
    parse: ParseWindows,
    log_failure: &impl Fn(&str, &ClientError),
) -> (QuotaReport, QuotaReport) {
    let request = |host| async move {
        let requested_at = now();
        let outcome = report(client, host, operation, token, body)
            .await
            .and_then(|report| parse(&report, requested_at).map_err(ClientError::UsageFormat));
        if let Err(error) = &outcome {
            log_failure(&format!("{host}/v1internal:{operation}"), error);
        }
        outcome
    };
    let daily = async {
        let first = request(DAILY).await?;
        // The daily host can alternate between measured quota and untouched copies.
        // Recheck an all-untouched summary before taking it as the account's reading.
        if operation == SUMMARY && !first.iter().any(QuotaWindow::has_activity) {
            for _ in 0..3 {
                if let Ok(retry) = request(DAILY).await
                    && retry.iter().any(QuotaWindow::has_activity)
                {
                    return Ok(retry);
                }
            }
        }
        Ok(first)
    };
    tokio::join!(daily, request(PRODUCTION))
}

fn user_agent() -> String {
    let platform = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        platform => platform,
    };
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        architecture => architecture,
    };
    // T3 Code's quota client identity. Google rejects the generic HTTP client and
    // older Antigravity versions can omit or misreport Gemini quota.
    format!(
        "antigravity/hub/2.24.0 (aidev_client; os_type={platform}; arch={architecture}; cl=963137146)"
    )
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
        .map_err(|error| {
            if error.is_timeout() {
                ClientError::TimedOut(Duration::from_secs(10))
            } else {
                usage_error("Google quota request failed")
            }
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(ClientError::Usage(format!(
            "Google quota request returned HTTP {status}"
        )));
    }
    response.json().await.map_err(|error| {
        if error.is_decode() {
            ClientError::UsageFormat("expected a JSON response body".into())
        } else {
            usage_error("could not read the Google quota response")
        }
    })
}

async fn account_credentials(launch: &AgentLaunch) -> Result<Value, ClientError> {
    let directory = match launch.antigravity_profile() {
        Some(profile) => std::path::PathBuf::from(profile),
        None => {
            if let Ok(credentials) = native_credentials().await {
                return Ok(credentials);
            }
            std::env::var_os("ANTIGRAVITY_GEMINI_DIR")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    ["USERPROFILE", "HOME"]
                        .into_iter()
                        .find_map(std::env::var_os)
                        .map(|home| std::path::PathBuf::from(home).join(".gemini"))
                })
                .ok_or_else(|| usage_error("agy has no profile directory"))?
        }
    };
    let folder = directory.join("antigravity-cli");
    for file in ["antigravity-oauth-token", "jetski-cli-oauth-token"] {
        if let Ok(bytes) = tokio::fs::read(folder.join(file)).await
            && let Ok(credentials) = serde_json::from_slice(&bytes)
        {
            return Ok(credentials);
        }
    }
    Err(usage_error("agy has no login in this profile"))
}

/// agy can create an ACP session before authenticating, so the isolated login must
/// be confirmed by the CLI's own token store rather than by session/new alone.
pub async fn antigravity_profile_identity(launch: &AgentLaunch) -> Option<String> {
    let credentials = account_credentials(launch).await.ok()?;
    ["email", "user_email", "user_tier", "user_tier_id"]
        .into_iter()
        .find_map(|key| credentials.get(key).and_then(Value::as_str))
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
}

pub async fn antigravity_profile_signed_in(launch: &AgentLaunch) -> bool {
    account_credentials(launch)
        .await
        .ok()
        .is_some_and(|credentials| access_token(&credentials).is_ok())
}

fn access_token(credentials: &Value) -> Result<&str, ClientError> {
    token_record(credentials)
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| usage_error("agy has no current Google access token"))
}

fn token_record(credentials: &Value) -> &Value {
    credentials.get("token").unwrap_or(credentials)
}

fn token_expired(credentials: &Value) -> bool {
    let expiry = token_record(credentials)
        .get("expiry")
        .and_then(Value::as_str)
        .and_then(unix_seconds);
    expiry.map_or(credentials.get("token").is_some(), |expiry| {
        expiry <= now() + 60
    })
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(windows)]
fn agy_credential_entry() -> keyring::Result<keyring::Entry> {
    // go-keyring's Windows target is service:user.
    keyring::Entry::new_with_target("gemini:antigravity", "gemini", "antigravity")
}

#[cfg(target_os = "linux")]
fn agy_credential_entry() -> keyring::Result<keyring::Entry> {
    // go-keyring's Secret Service item carries only service and username attributes, which
    // keyring finds through its fallback search of the default collection.
    keyring::Entry::new("gemini", "antigravity")
}

#[cfg(any(windows, target_os = "linux"))]
async fn native_credentials() -> Result<Value, ClientError> {
    tokio::task::spawn_blocking(|| {
        // Nothing is written back.
        let entry = agy_credential_entry()
            .map_err(|_| usage_error("could not open agy's credential store"))?;
        let bytes = entry
            .get_secret()
            .map_err(|_| usage_error("agy has no readable Google login"))?;
        serde_json::from_slice(&bytes).map_err(|_| usage_error("invalid agy credential record"))
    })
    .await
    .map_err(|_| usage_error("could not read agy's credential store"))?
}

#[cfg(not(any(windows, target_os = "linux")))]
async fn native_credentials() -> Result<Value, ClientError> {
    Err(usage_error("no agy ACP credential record"))
}

fn usage_error(message: &str) -> ClientError {
    // Response bodies and credential records can include private account data.
    ClientError::Usage(message.into())
}
