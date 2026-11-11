//! Prints the proof level each language reaches for the proof-list features a PR touches
//! (SPEC.md F17 Confidence proof), without building the proofs:
//!
//! `cargo run -p velocity-proofs --example proof_levels -- <worktree> <base> <head>`

use std::path::PathBuf;

use velocity_proofs::ProofCheckReport;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [worktree, base, head] = arguments.as_slice() else {
        return Err("usage: proof_levels <worktree> <base> <head>".into());
    };
    let levels = velocity_proofs::levels_between(&PathBuf::from(worktree), base, head)?;
    let mut report = ProofCheckReport::nothing_changed(Some("proofs".into()));
    report.levels = levels;
    match report.summary() {
        Some(summary) => println!("{summary}"),
        None => println!("{base}...{head} touches no proof-list feature."),
    }
    Ok(())
}
