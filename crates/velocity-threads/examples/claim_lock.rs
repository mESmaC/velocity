//! Headless F14/F16 claim lock driver over the same piece derivation and channel calls
//! the app's `airc_claims::attach` makes: `cargo run -p velocity-threads --example claim_lock`.
use velocity_airc::{Channel, ClaimResult};
use velocity_store::ThreadItem;
use velocity_store::claim_piece::thread_claim_piece;
use velocity_store::issues::ThreadIssue;

fn claim(channel: &Channel, thread: &ThreadItem, branch: &str) -> Result<ClaimResult, String> {
    let member = format!("thread-{}", thread.id);
    channel
        .join(&member, branch.trim_start_matches("vel/"))
        .map_err(|error| error.to_string())?;
    channel
        .claim_piece(
            &member,
            &thread_claim_piece(thread, branch),
            &thread.title,
            branch,
            Vec::new(),
        )
        .map_err(|error| error.to_string())
}

fn show(label: &str, result: &ClaimResult) {
    match result {
        ClaimResult::Granted { claim } => println!("{label}: granted {}", claim.piece),
        ClaimResult::Refused { holder, .. } => println!("{label}: refused, held by {holder}"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("velocity-claim-lock-{stamp}"));
    let channel = Channel::open(&directory, "project")?;

    // #100: two composer threads whose title model picked the same title.
    let first = ThreadItem::new(1, "Fix failing tests".into());
    let second = ThreadItem::new(2, "Fix failing tests".into());
    show(
        "composer thread 1",
        &claim(&channel, &first, "vel/fix-tests")?,
    );
    show(
        "composer thread 2",
        &claim(&channel, &second, "vel/fix-tests-2")?,
    );

    // A dispatched issue thread, then a goal swarm agent naming the same issue its own way.
    let mut dispatched = ThreadItem::new(3, "Slash menu omits agent commands".into());
    dispatched.issue = Some(ThreadIssue::new("Owner/Repo".into(), 33));
    show(
        "dispatched #33",
        &claim(&channel, &dispatched, "vel/issue-33")?,
    );
    channel.join("goal-sol-1", "sol-1")?;
    let goal = channel.claim_piece(
        "goal-sol-1",
        "Issue: #33",
        "Fix the slash menu",
        "vel/issue-33-2",
        vec![],
    )?;
    show("goal agent issue:#33", &goal);

    // A swarm member on Task List item 7 against an agent that claimed it first.
    channel.join("goal-argon-1", "argon-1")?;
    show(
        "goal agent task:7",
        &channel.claim_piece("goal-argon-1", "task:7", "Task 7", "vel/task-7", vec![])?,
    );
    let mut member = ThreadItem::new(4, "Task 7".into());
    member.task_ids.push(7);
    show(
        "swarm member task 7",
        &claim(&channel, &member, "vel/task-7-2")?,
    );

    for entry in channel
        .read(0)?
        .iter()
        .filter(|entry| entry.kind == "refused")
    {
        println!("{} {}", entry.kind, entry.text);
    }
    // A failed or departed holder keeps its piece until it is released.
    channel.leave("goal-argon-1")?;
    show(
        "swarm member after holder left",
        &claim(&channel, &member, "vel/task-7-2")?,
    );
    channel.release("goal-argon-1", "task:7")?;
    show(
        "swarm member after release",
        &claim(&channel, &member, "vel/task-7-2")?,
    );
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
