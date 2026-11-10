//! Run F14 comment routing and durable delivery without a desktop or provider account.
use velocity_airc::{Channel, deliver_messages};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let folder = std::env::temp_dir().join(format!("velocity-delivery-{}", std::process::id()));
    let channel = Channel::open(&folder, "headless")?;
    channel.comment("Historical comment before these threads existed")?;
    let first = channel.join("first", r"C:\work trees\editor")?;
    let second = channel.join("second", r"C:\work trees\builder")?;
    channel.message("second", vec![first.clone()], "question", "Which files?")?;
    channel.comment(&format!("{first}: Keep the saved record"))?;
    channel.comment("Please record progress")?;
    let mut pending = Vec::new();
    let cursor = deliver_messages(
        &channel,
        &first,
        channel.delivery_cursor("first", 0)?,
        |identifier, message| {
            pending.push((identifier.to_owned(), message.to_owned()));
            Ok(false)
        },
    )?;
    if pending.len() != 1 {
        return Err("pending delivery was skipped".into());
    }
    let reopened = Channel::open(&folder, "headless")?;
    let mut completed = Vec::new();
    let acknowledged = deliver_messages(&reopened, &first, cursor, |identifier, message| {
        println!("{identifier}: {message}");
        completed.push(identifier.to_owned());
        Ok(true)
    })?;
    if completed.len() != 3 || completed.first() != pending.first().map(|item| &item.0) {
        return Err("restart lost or misrouted a message".into());
    }
    deliver_messages(&channel, &first, acknowledged, |_, _| {
        Err("replayed acknowledged line".into())
    })?;
    let mut others = 0;
    deliver_messages(
        &channel,
        &second,
        channel.delivery_cursor("second", 0)?,
        |_, _| {
            others += 1;
            Ok(true)
        },
    )?;
    if others != 1 {
        return Err("targeted message reached another agent".into());
    }
    if channel.comment("unknown: do not broadcast").is_ok() {
        return Err("unknown recipient broadcast".into());
    }
    println!("Headless delivery passed: pending replay, acknowledgement, targeting and broadcast.");
    // Keep the temporary log available as evidence; no user files are touched.
    println!("Channel evidence: {}", folder.display());
    Ok(())
}
