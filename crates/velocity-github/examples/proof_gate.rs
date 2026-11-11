//! Headless F17 gate driver. Usage: proof_gate <review-worktree> <base> <head>.
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.len() != 3 {
        return Err("usage: proof_gate <review-worktree> <base> <head>".into());
    }
    velocity_github::check_proof_gate(Path::new(&arguments[0]), &arguments[1], &arguments[2])?;
    println!("Proof gate passed for {}", arguments[2]);
    Ok(())
}
