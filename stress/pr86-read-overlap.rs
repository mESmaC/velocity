// Risk: F14 overlaps mean "two agents claim the same file". Every agent reads AGENTS.md
// and SPEC.md first, so if a Read or Search tool call reached the overlap check, every
// pair of agents would get an overlap line for files neither of them edits.
// A model-generated thread title must not be the piece key either: two threads whose
// titles match would refuse each other, and the second would fail with
// "Could not claim work" (#100).
// Updated for #230: the build-thread pump now feeds `observe_file` only the paths
// `velocity_acp::tool_mutations::ToolMutations` returns (src-tauri/src/airc_claims.rs
// `observe`), and a thread's lock key comes from
// `velocity_store::claim_piece::thread_claim_piece` (src-tauri/src/airc_claims.rs
// `attach`). Both checks drive those entry points; the assertions are unchanged.
use agent_client_protocol::v1::schema::SessionUpdate;
use velocity_acp::tool_mutations::ToolMutations;
use velocity_airc::{Channel, ClaimResult};
use velocity_store::ThreadItem;
use velocity_store::claim_piece::thread_claim_piece;

fn report(name: &str, passed: bool, detail: &str) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

/// What the pump does for one tool call: classify it, then observe every path it
/// returns, relative to the worktree.
fn pump(
    channel: &Channel,
    member: &str,
    tools: &mut ToolMutations,
    kind: &str,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let update: SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate": "tool_call", "toolCallId": format!("{member}-{kind}"),
        "title": format!("{kind} {path}"), "kind": kind, "locations": [{"path": path}]
    }))?;
    for observed in tools.update(&update) {
        channel.observe_file(member, &observed.display().to_string())?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("velocity-pr86-{stamp}"));
    let channel = Channel::open(&directory, "project")?;
    channel.join("thread-1", r"X:\velocity-wt\command-palette")?;
    channel.join("thread-2", r"X:\velocity-wt\thread-ipc")?;
    channel.claim("thread-1", "Command palette", "vel/command-palette", Vec::new())?;
    channel.claim("thread-2", "Thread IPC", "vel/thread-ipc", Vec::new())?;

    // Both agents read and search AGENTS.md, as every agent does first.
    for member in ["thread-1", "thread-2"] {
        let mut tools = ToolMutations::default();
        pump(&channel, member, &mut tools, "read", "AGENTS.md")?;
        pump(&channel, member, &mut tools, "search", "AGENTS.md")?;
    }
    let overlaps: Vec<_> = channel
        .read(0)?
        .into_iter()
        .filter(|entry| entry.kind == "overlap")
        .map(|entry| entry.text)
        .collect();
    let mut failures = report(
        "reading a file posts no overlap",
        overlaps.is_empty(),
        &format!("{overlaps:?}"),
    );

    // Two build threads whose title model picked the same title.
    let first = ThreadItem::new(3, "Fix failing tests".into());
    let second = ThreadItem::new(4, "Fix failing tests".into());
    channel.join("thread-3", r"X:\velocity-wt\fix-tests")?;
    channel.join("thread-4", r"X:\velocity-wt\fix-tests-2")?;
    let piece = thread_claim_piece(&first, "vel/fix-tests");
    channel.claim_piece("thread-3", &piece, &first.title, "vel/fix-tests", Vec::new())?;
    let piece = thread_claim_piece(&second, "vel/fix-tests-2");
    let second =
        channel.claim_piece("thread-4", &piece, &second.title, "vel/fix-tests-2", Vec::new())?;
    failures += report(
        "distinct threads with one title both start",
        matches!(second, ClaimResult::Granted { .. }),
        &format!("{second:?}"),
    );
    std::fs::remove_dir_all(directory)?;
    std::process::exit(i32::from(failures > 0));
}
