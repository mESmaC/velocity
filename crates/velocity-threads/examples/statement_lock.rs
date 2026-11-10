//! F17 statement lock without GitHub: what a PR duty review does before its agent's turn when
//! the PR was approved at an earlier head.
//!
//! cargo run -p velocity-threads --example statement_lock -- <worktree> <base> <approved head> <new head>
//!
//! Records an approval of PR #1 at `<approved head>` in a scratch approval record, locks the
//! theorem statements there as the PR duty MCP server does on approve, then runs the same
//! checks a review of `<new head>` runs, and prints the prompt section and the approval's
//! standing before and after.
use std::path::PathBuf;

use velocity_airc::{Channel, Event};
use velocity_github::ReviewRecord;
use velocity_proofs::{save_statement_lock, statements_at};
use velocity_threads::proof_checks::{ProofCheckTarget, check_before_review};

const PROJECT: &str = "1";
const NUMBER: u32 = 1;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [worktree, base, approved, head] = arguments.as_slice() else {
        return Err("usage: statement_lock <worktree> <base> <approved head> <new head>".into());
    };
    let worktree = PathBuf::from(worktree);
    let app_data =
        std::env::temp_dir().join(format!("velocity-statement-lock-{}", std::process::id()));
    let channel = Channel::open(&app_data, PROJECT)?;
    channel.post_event(
        Event::new("approved", "pr-1", "approved #1").for_pull_request(NUMBER, Some(approved)),
    )?;
    save_statement_lock(
        &app_data,
        PROJECT,
        NUMBER,
        &statements_at(&worktree, approved)?,
    )?;
    let record = ReviewRecord::open(&app_data, PROJECT, "pr-1")?;
    println!(
        "standing at the approved head before: {:?}",
        record.standings(&[(NUMBER, approved)])?
    );
    let section = check_before_review(&ProofCheckTarget {
        app_data: &app_data,
        project: PROJECT,
        nickname: "pr-1",
        number: NUMBER,
        worktree: &worktree,
        base_commit: base,
        head,
    });
    println!("prompt section:{section}");
    println!(
        "standing at the approved head after: {:?}",
        record.standings(&[(NUMBER, approved)])?
    );
    for entry in channel.read(0)? {
        println!("airc  {} {} {}", entry.kind, entry.nick, entry.text);
    }
    std::fs::remove_dir_all(&app_data)?;
    Ok(())
}
