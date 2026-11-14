// Risk (#266): the F17 design-proof gate accepts a Design.lean only when it names the
// SPEC.md section it implements. Widening the label rule for other projects must not
// reject the section references Velocity's own proofs already use (`F6/F14.`,
// `(§2.2)`, `F5/F12, F17`), or PR duty refuses every proof-touching PR here.
// Checks the real velocity-proofs library against every proofs/*/Design.lean header
// and the reference spellings found in them.
// Run: `cargo run -p stress -- pr266-own-proof-sections`.
use std::fs;
use velocity_proofs::names_spec_section;

fn report(name: &str, passed: bool) -> usize {
    println!("{} {name}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn main() {
    let mut failures = 0;
    for spelling in [
        "-- SPEC.md F6/F14. Native edges supply overlap.",
        "-- SPEC.md F5/F12, F17 worktree deletion only.",
        "-- Implements SPEC.md (§2.2) stall detection.",
        "-- SPEC.md (F14) claim lock.",
        "-- SPEC.md §2.2 and F17.",
        "-- SPEC.md R8: rule eight.",
    ] {
        failures += report(spelling, names_spec_section(spelling));
    }
    for rejected in ["-- SPEC.md", "-- SPEC.md section two", "-- section 2.2"] {
        failures += report(&format!("rejects {rejected}"), !names_spec_section(rejected));
    }
    let Ok(folders) = fs::read_dir("proofs") else {
        println!("FAIL no proofs folder in {:?}", std::env::current_dir());
        std::process::exit(1);
    };
    for folder in folders.flatten() {
        let design = folder.path().join("Design.lean");
        if let Ok(text) = fs::read_to_string(&design) {
            failures += report(&design.display().to_string(), names_spec_section(&text));
        }
    }
    std::process::exit(i32::from(failures > 0));
}
