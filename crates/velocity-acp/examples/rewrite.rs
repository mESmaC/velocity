//! Run one production rewrite without opening the desktop app.
use velocity_acp::client::{AgentLaunch, SessionSetup};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let directory = arguments.next().ok_or("Pass a checkout directory")?;
    let prompt = arguments.next().ok_or("Pass the rewrite prompt")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let launch = AgentLaunch::new("claude-code-acp".into());
    let setup = SessionSetup::new(directory.into(), vec!["bypassPermissions".into()]);
    let response = runtime.block_on(velocity_acp::client::rewrite::rewrite(
        launch, setup, prompt,
    ))?;
    println!("{response}");
    Ok(())
}
