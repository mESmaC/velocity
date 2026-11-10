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
    if let (Some(worktree), Some(repository)) = (arguments.get(1), arguments.get(3)) {
        rebase_demo(repository, worktree)?;
    }
    Ok(())
}

fn rebase_demo(repository: &str, worktree: &str) -> Result<(), Box<dyn std::error::Error>> {
    let directory =
        std::env::temp_dir().join(format!("velocity-rebase-demo-{}", std::process::id()));
    let channel = velocity_airc::Channel::open(&directory, "demo")?;
    channel.join("demo", "demo")?;
    let branch = velocity_git::current_branch(std::path::Path::new(worktree))?;
    channel.register_checkout(
        "demo",
        velocity_airc::waits::Checkout::new(
            repository.into(),
            worktree.into(),
            branch,
            "master".into(),
        ),
    )?;
    let result = channel.rebase("demo");
    for event in channel.events(0)? {
        println!("{}: {}", event.kind, event.text);
    }
    result?;
    Ok(())
}
