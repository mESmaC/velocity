// Risk: F14 overlaps mean "two agents claim the same file". The build-thread pump in
// src-tauri/src/airc_claims.rs passes every ACP tool-call location to `observe_file`,
// including Read and Search calls. Every agent reads AGENTS.md and SPEC.md first, so
// every pair of agents would get an overlap line for files neither of them edits.
// A model-generated thread title is also the piece key, so two threads with the same
// title refuse each other and the second fails with "Could not claim work".
// Run against the real velocity-airc library: copy into crates/velocity-airc/examples/ and
// `cargo run -p velocity-airc --example pr86-read-overlap`.
use velocity_airc::{Channel, ClaimResult};

fn report(name: &str, passed: bool, detail: &str) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
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

    // What the pump does for a Read tool call whose location is AGENTS.md.
    channel.observe_file("thread-1", "AGENTS.md")?;
    channel.observe_file("thread-2", "AGENTS.md")?;
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
    channel.join("thread-3", r"X:\velocity-wt\fix-tests")?;
    channel.join("thread-4", r"X:\velocity-wt\fix-tests-2")?;
    channel.claim("thread-3", "Fix failing tests", "vel/fix-tests", Vec::new())?;
    let second = channel.claim("thread-4", "Fix failing tests", "vel/fix-tests-2", Vec::new())?;
    failures += report(
        "distinct threads with one title both start",
        matches!(second, ClaimResult::Granted { .. }),
        &format!("{second:?}"),
    );
    std::fs::remove_dir_all(directory)?;
    std::process::exit(i32::from(failures > 0));
}
