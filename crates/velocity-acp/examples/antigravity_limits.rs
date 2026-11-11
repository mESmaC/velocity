//! Run the same quota source as Settings → Providers without starting an agent turn.

use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(path) = std::env::args_os().nth(1) {
        let report = serde_json::from_slice(&std::fs::read(path)?)?;
        let reading = velocity_acp::antigravity_limits::antigravity_usage_reading(&report)
            .or_else(|| velocity_acp::antigravity_limits::antigravity_model_reading(&report));
        println!("{reading:?}");
        return Ok(());
    }
    let launch = velocity_acp::provider_agent(velocity_acp::Provider::Antigravity).launch;
    let reading = velocity_acp::client::read_antigravity_usage(
        &launch,
        &std::env::current_dir()?,
        Duration::from_secs(60),
    )
    .await?;
    println!("{reading:?}");
    Ok(())
}
