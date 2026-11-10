//! Headless claims driver: cargo run -p velocity-airc --example claims -- <app-data>.
use std::path::PathBuf;
use velocity_airc::Channel;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("Pass app-data directory")?,
    );
    let channel = Channel::open(&data, "claims-demo")?;
    channel.join("first", "first-agent")?;
    channel.join("second", "second-agent")?;
    println!(
        "{}",
        serde_json::to_string(&channel.claim(
            "first",
            "Build composer",
            "vel/composer",
            vec!["ui/src/Composer.svelte".into()]
        )?)?
    );
    println!(
        "{}",
        serde_json::to_string(&channel.claim(
            "second",
            "Build composer",
            "vel/other",
            Vec::new()
        )?)?
    );
    channel.claim(
        "second",
        "Build sidebar",
        "vel/sidebar",
        vec!["ui/src/Composer.svelte".into(), "Cargo.lock".into()],
    )?;
    channel.share("second", "first-agent", "ui/src/Composer.svelte")?;
    channel.leave("first")?;
    println!("Held after leave: {}", channel.claims()?.len());
    for event in channel.read(0)? {
        println!("{} {} {}", event.kind, event.member, event.text);
    }
    channel.release("first", "Build composer")?;
    channel.release("second", "Build sidebar")?;
    Ok(())
}
