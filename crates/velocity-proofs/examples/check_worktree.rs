//! Runs Velocity's proof checks on a checkout, as PR duty does before a review:
//!
//! `cargo run -p velocity-proofs --example check_worktree -- <worktree> <base> <head> [<locked head>]`
//!
//! Prints the checks PR duty's prompt carries, the proof card a PR thread shows (F17), and,
//! given an earlier head, the theorem statements changed since it (the statement lock).

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [worktree, base, head, rest @ ..] = arguments.as_slice() else {
        return Err("usage: check_worktree <worktree> <base> <head> [<locked head>]".into());
    };
    let worktree = PathBuf::from(worktree);
    let report = velocity_proofs::check_worktree(&worktree, base, head)?;
    match report.summary() {
        Some(summary) => println!("{summary}"),
        None => println!("No proof files changed between {base} and {head}."),
    }
    println!("passed: {}", report.passed());
    match velocity_proofs::proof_card(&worktree, base, head, None)? {
        Some(card) => println!(
            "proof card ({}):
{card:#?}",
            card.summary()
        ),
        None => println!("No proof card: the PR changes no proof file or fixture table."),
    }
    let current = velocity_proofs::statements_at(&worktree, head)?;
    println!(
        "{} theorem statements locked at {head}",
        current.statements.len()
    );
    if let Some(locked_head) = rest.first() {
        let locked = velocity_proofs::statements_at(&worktree, locked_head)?;
        let changed = locked.changed_in(&current);
        println!("changed since {locked_head}: {changed:?}");
    }
    Ok(())
}
