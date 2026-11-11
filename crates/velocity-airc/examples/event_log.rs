//! Headless verification of the F14 event log posted by Velocity from claims,
//! GitHub and the Task List, with agent messages and Past work kept separate.
use velocity_airc::{Channel, Event};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("velocity-airc-events-{}", std::process::id()));
    let channel = Channel::open(&root, "project-1")?;
    let builder = channel.join("thread-1", r"X:\velocity-wt\airc-event-log")?;
    let reviewer = channel.join("duty-pr-1", "pr-1")?;

    // 1. Velocity posts claim, Task List, GitHub PR lifecycle, scout issue and failure events.
    channel.claim(
        "thread-1",
        "Event log posted by Velocity",
        "vel/event-log",
        vec!["crates/velocity-airc/src/event_log.rs".into()],
    )?;
    channel.post_event_once(Event::task_saved("you", 1, "Wire AIRC event log")?)?;
    channel.post_event_once(Event::pull_request_opened(
        &builder,
        89,
        "Event log posted by Velocity",
        "1111111aaaa",
    )?)?;
    // Polling the same open PR head again does not duplicate the PR event.
    if channel
        .post_event_once(Event::pull_request_opened(
            &builder,
            89,
            "Event log posted by Velocity",
            "1111111aaaa",
        )?)?
        .is_some()
    {
        return Err("duplicate PR opened event was posted".into());
    }
    channel.post_event_once(Event::pull_request_pushed(&builder, 89, "2222222bbbb")?)?;
    channel.post_event_once(Event::issue_filed(
        "scout-1",
        57,
        "Ctrl+K palette needs fuzzy match",
    )?)?;
    channel.post_event_once(Event::thread_failed("gh-prs", "gh is not authenticated")?)?;

    // 2. Agent-to-agent messages and discoveries happen alongside events.
    channel.message(
        "thread-1",
        vec![reviewer.clone()],
        "inform",
        "Ready for review at 2222222.",
    )?;
    channel.append_ledger(
        "thread-1",
        "discovery",
        "remove_dir_all fails on Windows while a handle is open",
    )?;

    // 3. Merging posts a `merged` event AND appends a `past_work` ledger entry once.
    channel.post_pull_request_merged(
        &builder,
        89,
        "Event log posted by Velocity",
        "2222222bbbb",
    )?;
    if channel
        .post_pull_request_merged(&builder, 89, "Event log posted by Velocity", "2222222bbbb")?
        .is_some()
    {
        return Err("duplicate merged event was posted".into());
    }

    let events = channel.events(0)?;
    let messages = channel.messages(0)?;
    let discoveries = channel.ledger("discovery", 0)?;
    let past_work = channel.ledger("past_work", 0)?;

    let tags: Vec<&str> = events.iter().map(|entry| entry.kind.as_str()).collect();
    if tags
        != vec![
            "claim",
            "task",
            "PR",
            "pushed",
            "issue",
            "failed",
            "discovery",
            "merged",
        ]
    {
        return Err(format!("unexpected event tags: {tags:?}").into());
    }
    if events
        .iter()
        .any(|entry| entry.kind == "message" || entry.kind == "past_work")
    {
        return Err("events feed leaked agent messages or past_work".into());
    }
    if messages.len() != 1 || messages[0].text != "Ready for review at 2222222." {
        return Err(format!("unexpected messages feed: {messages:?}").into());
    }
    if discoveries.len() != 1 || past_work.len() != 1 {
        return Err("unexpected ledger counts".into());
    }
    println!(
        "Verified {} events, {} message, {} discovery, {} past_work entry.",
        events.len(),
        messages.len(),
        discoveries.len(),
        past_work.len()
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
