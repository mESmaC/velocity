//! Headless F6 driver: inspect real worktree region overlaps and landing decisions.
use velocity_core::landing::{check_scope, needs_rebase};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().collect();
    if let (Some(path), Some(commit)) = (arguments.get(1), arguments.get(2)) {
        let overlaps = velocity_git::landing::edited_overlap(std::path::Path::new(path), commit)?;
        println!("Region overlaps: {overlaps:?}");
    }
    for (same_head, same_base, overlap) in [
        (true, true, false),
        (true, false, false),
        (true, false, true),
        (false, true, false),
    ] {
        println!(
            "head unchanged={same_head}, base unchanged={same_base}, overlap={overlap}: {:?}",
            check_scope(same_head, same_base, overlap)
        );
    }
    println!(
        "claimed rebase: {}; generated-only rebase: {}",
        needs_rebase(true, false, true),
        needs_rebase(true, false, false)
    );
    Ok(())
}
