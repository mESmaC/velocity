//! Headless F14 delivery driver; leaves its log for inspection.
use velocity_airc::{Channel, Event, MessageMode, deliver_targeted};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let folder = std::env::temp_dir().join(format!("velocity-routing-{}", std::process::id()));
    let channel = Channel::open(&folder, "routing")?;
    let builder = channel.join("builder", "builder")?;
    let reviewer = channel.join("reviewer", "reviewer")?;
    channel.claim("builder", "Build", "vel/build", vec!["src/lib.rs".into()])?;
    channel.set_message_mode(&builder, MessageMode::HeadsDown)?;
    let cursor = channel.delivery_cursor("builder", 0)?;
    channel.message(
        "reviewer",
        vec![builder.clone()],
        "question",
        "Which files?",
    )?;
    channel.post_event(Event::new("changes", &builder, "Review needs a fix"))?;
    channel.comment(&format!("{builder}: Keep working"))?;
    println!("Heads-down while busy (event and user comment):");
    let cursor = drain(&channel, &builder, cursor, false)?;
    channel.post_event(Event::pull_request_pushed(&builder, 42, "abc1234")?)?;
    println!("PR update releases held question while still busy:");
    let cursor = drain(&channel, &builder, cursor, false)?;
    println!("Idle after acknowledgement:");
    let cursor = drain(&channel, &builder, cursor, true)?;
    channel.set_message_mode(&reviewer, MessageMode::Quarantined)?;
    channel.message(
        "reviewer",
        vec![builder.clone()],
        "inform",
        "Quarantined output",
    )?;
    channel.set_message_mode(&reviewer, MessageMode::Open)?;
    channel.set_message_mode(&builder, MessageMode::Quarantined)?;
    println!("Frozen claim: {:?}", channel.release("builder", "Build"));
    channel.comment(&format!("{builder}: You can still hear me"))?;
    drain(&channel, &builder, cursor, true)?;
    println!("Channel evidence: {}", folder.display());
    Ok(())
}
fn drain(
    channel: &Channel,
    nick: &str,
    cursor: u64,
    idle: bool,
) -> Result<u64, velocity_airc::Error> {
    deliver_targeted(channel, nick, cursor, idle, |_, message| {
        println!("{message}");
        Ok(true)
    })
}
