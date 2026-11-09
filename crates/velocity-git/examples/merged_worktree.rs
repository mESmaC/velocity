//! Execute the F6 cleanup edge against an explicitly supplied worktree.
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().collect();
    let repository = Path::new(arguments.get(1).ok_or("expected repository")?);
    let worktree = Path::new(arguments.get(2).ok_or("expected worktree")?);
    let branch = arguments.get(3).ok_or("expected branch")?;
    velocity_git::remove_merged_worktree(repository, worktree, branch, true)?;
    println!("Removed the merged worktree and its local branch");
    Ok(())
}
