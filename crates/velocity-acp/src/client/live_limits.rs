//! Live provider reports stay informational and never enter the transcript or activity state.
use std::path::PathBuf;
use std::sync::OnceLock;

use agent_client_protocol::v1::schema::SessionUpdate;
use tokio::sync::broadcast;

use super::AgentLaunch;
use crate::Provider;
use crate::usage_limits::{LimitReading, claude_limit_event, codex_usage_reading};

static REPORTS: OnceLock<broadcast::Sender<(Provider, Option<LimitReading>)>> = OnceLock::new();
static BRIDGE: tokio::sync::OnceCell<PathBuf> = tokio::sync::OnceCell::const_new();

/// Subscribe before starting agents. Slow consumers recover on the next report/read.
pub fn subscribe_limits() -> broadcast::Receiver<(Provider, Option<LimitReading>)> {
    REPORTS
        .get_or_init(|| broadcast::channel(128).0)
        .subscribe()
}

fn publish(provider: Provider, reading: Option<LimitReading>) {
    if let Some(reports) = REPORTS.get() {
        let _ = reports.send((provider, reading));
    }
}

pub(super) fn observe(update: &SessionUpdate) {
    if let SessionUpdate::UsageUpdate(usage) = update
        && let Some(report) = usage
            .meta
            .as_ref()
            .and_then(|meta| meta.get("_claude/rateLimit"))
    {
        publish(Provider::Anthropic, claude_limit_event(report));
    }
}

pub(super) fn observe_log(text: &str) -> bool {
    let Some(report) = text.strip_prefix("velocity:codex-limit ") else {
        return false;
    };
    if let Ok(report) = serde_json::from_str(report) {
        publish(Provider::Codex, codex_usage_reading(&report));
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
