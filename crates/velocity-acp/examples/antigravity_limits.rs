//! Run the same quota source as Settings → Providers without starting an agent turn.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(path) = std::env::args_os().nth(1) {
        let requested_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        let report = serde_json::from_slice(&std::fs::read(path)?)?;
        let reading =
            velocity_acp::antigravity_limits::antigravity_usage_reading(&report, requested_at)?;
        println!("{reading:?}");
        return Ok(());
    }
    let launch = velocity_acp::provider_agent(velocity_acp::Provider::Antigravity)
        .ok_or("Antigravity has no built-in agent command")?
        .launch;
    let reading = velocity_acp::client::read_antigravity_usage(
        &launch,
        &std::env::current_dir()?,
        Duration::from_secs(60),
        |request, error| eprintln!("{request}: {error}"),
    )
    .await?;
    println!("{reading:?}");
    Ok(())
}
