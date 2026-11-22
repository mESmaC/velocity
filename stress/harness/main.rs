//! Every Rust stress harness in one binary (#499), so the suite builds them all at once in
//! the root workspace. `harness/build.rs` makes each harness a module; this picks one by the
//! name in `VELOCITY_STRESS_HARNESS` and runs its `main`. A variable rather than an argument
//! lets each harness read its own arguments unchanged.

use std::process::ExitCode;

include!(concat!(env!("OUT_DIR"), "/harness/harnesses.rs"));

const HARNESS_VARIABLE: &str = "VELOCITY_STRESS_HARNESS";

fn main() -> ExitCode {
    let name = std::env::var(HARNESS_VARIABLE).unwrap_or_default();
    match HARNESSES.iter().find(|(harness, _)| *harness == name) {
        Some((_, run)) => run(),
        None => {
            let known: Vec<&str> = HARNESSES.iter().map(|(harness, _)| *harness).collect();
            eprintln!(
                "stress-harness: set {HARNESS_VARIABLE} to one of: {}; it was {name:?}",
                known.join(", ")
            );
            ExitCode::FAILURE
        }
    }
}
