//! Live provider reports stay informational and never enter the transcript or activity state.
use std::path::PathBuf;
use std::sync::OnceLock;

use agent_client_protocol::v1::schema::SessionUpdate;
use tokio::sync::broadcast;

use super::AgentLaunch;
use crate::Provider;
use crate::usage_limits::{LimitReading, claude_limit_event, codex_usage_reading};

static REPORTS: OnceLock<broadcast::Sender<(Provider, Option<LimitReading>)>> = OnceLock::new();
type AccountReport = (Provider, u32, Option<LimitReading>);
static ACCOUNT_REPORTS: OnceLock<broadcast::Sender<AccountReport>> = OnceLock::new();
static BRIDGE: tokio::sync::OnceCell<PathBuf> = tokio::sync::OnceCell::const_new();

pub fn subscription_limit_error(error: &str) -> bool {
    let message = error.to_ascii_lowercase();
    [
        "usage limit",
        "subscription limit",
        "quota exceeded",
        "hit your limit",
        "usage_limit_reached",
        "quota exhausted",
        "usage_limit_exceeded",
        "usagelimitexceeded",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

pub(super) fn turn_error(error: &agent_client_protocol::Error) -> String {
    // Claude Code reports the reset in structured error data even when it has no
    // utilization to send in the ordinary usage frame.
    let rejected = error
        .data
        .as_ref()
        .and_then(|data| data.get("rateLimit"))
        .filter(|limit| {
            limit.get("status").and_then(serde_json::Value::as_str) == Some("rejected")
        });
    if let Some(limit) = rejected {
        let mut message = format!("Subscription limit reached · {}", error.message);
        if let Some(reset) = limit.get("resetsAt").and_then(serde_json::Value::as_i64) {
            message.push_str(&format!(" · resetsAt {reset}"));
        }
        message
    } else {
        error.to_string()
    }
}

/// Subscribe before starting agents. Slow consumers recover on the next report/read.
pub fn subscribe_limits() -> broadcast::Receiver<(Provider, Option<LimitReading>)> {
    REPORTS
        .get_or_init(|| broadcast::channel(128).0)
        .subscribe()
}

pub fn subscribe_account_limits() -> broadcast::Receiver<AccountReport> {
    ACCOUNT_REPORTS
        .get_or_init(|| broadcast::channel(128).0)
        .subscribe()
}

fn publish(provider: Provider, account: u32, reading: Option<LimitReading>) {
    if let Some(reports) = ACCOUNT_REPORTS.get() {
        let _ = reports.send((provider, account, reading.clone()));
    }
    if account != 0 {
        return;
    }
    if let Some(reports) = REPORTS.get() {
        let _ = reports.send((provider, reading));
    }
}

pub(super) fn observe_account(account: u32, update: &SessionUpdate) {
    if let SessionUpdate::UsageUpdate(usage) = update
        && let Some(report) = usage
            .meta
            .as_ref()
            .and_then(|meta| meta.get("_claude/rateLimit"))
    {
        publish(Provider::Anthropic, account, claude_limit_event(report));
    }
}

pub(super) fn observe_account_log(account: u32, text: &str) -> bool {
    let Some(report) = text.strip_prefix("velocity:codex-limit ") else {
        return false;
    };
    if let Ok(report) = serde_json::from_str(report) {
        publish(Provider::Codex, account, codex_usage_reading(&report));
    }
    true
}

/// Preload only in Codex's adapter process; NODE_OPTIONS is inherited by its Node children.
/// No adapter installation is modified. The owned process tree stops the bridge with ACP.
pub(super) async fn prepare(launch: &mut AgentLaunch) -> std::io::Result<()> {
    if launch.own_config != Some(Provider::Codex) {
        return Ok(());
    }
    let path = BRIDGE.get_or_try_init(write_bridge).await?;
    let options = launch
        .env
        .iter()
        .find(|(name, _)| name == "NODE_OPTIONS")
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| std::env::var("NODE_OPTIONS").unwrap_or_default());
    launch.env.retain(|(name, _)| name != "NODE_OPTIONS");
    launch.env.push((
        "NODE_OPTIONS".into(),
        format!(
            "{options} --require {}",
            serde_json::to_string(&path.to_string_lossy().replace('\\', "/"))?
        ),
    ));
    Ok(())
}

async fn write_bridge() -> std::io::Result<PathBuf> {
    // A unique directory and one initializer prevent simultaneous agent launches from
    // reading a partially written preloader, including other Velocity app processes.
    let instant = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("velocity-limits-{}-{instant}", std::process::id()));
    tokio::fs::create_dir(&directory).await?;
    let path = directory.join("codex.cjs");
    tokio::fs::write(&path, include_str!("codex_limits.cjs")).await?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v1::schema::UsageUpdate;
    use serde_json::json;

    // #130: real turn reports must reach the meter bus; context-token updates must not.
    #[test]
    fn live_turn_reports_reach_meter() -> Result<(), Box<dyn std::error::Error>> {
        let mut reports = subscribe_limits();
        observe_account(0, &SessionUpdate::UsageUpdate(UsageUpdate::new(90, 100)));
        assert!(matches!(
            reports.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let metadata: agent_client_protocol::v1::schema::Meta =
            serde_json::from_value(json!({"_claude/rateLimit": {
                "status": "allowed", "rateLimitType": "seven_day",
                "unifiedWindows": {"seven_day": {"utilization": 0.33, "resetsAt": 1794542400}}
            }}))?;
        observe_account(
            0,
            &SessionUpdate::UsageUpdate(UsageUpdate::new(90, 100).meta(metadata)),
        );
        let (provider, reading) = reports.try_recv()?;
        let reading = reading.ok_or("missing Claude reading")?;
        assert_eq!(
            (provider, reading.percent_used, reading.window.as_str()),
            (Provider::Anthropic, 33, "weekly")
        );
        assert_eq!(reading.resets_at, Some(1794542400));
        assert!(observe_account_log(
            0,
            r#"velocity:codex-limit {"rateLimits":{"primary":{"usedPercent":21,"windowDurationMins":10080,"resetsAt":1794857720},"planType":"pro"}}"#
        ));
        let (provider, reading) = reports.try_recv()?;
        let reading = reading.ok_or("missing Codex reading")?;
        assert_eq!(
            (provider, reading.percent_used, reading.plan.as_deref()),
            (Provider::Codex, 21, Some("Pro"))
        );
        assert!(observe_account_log(
            0,
            r#"velocity:codex-limit {"rateLimits":null}"#
        ));
        assert_eq!(reports.try_recv()?, (Provider::Codex, None));
        Ok(())
    }
}
