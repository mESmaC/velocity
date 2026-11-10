//! F7 Tiered review without a window: tiers a head against a base the way a PR duty review
//! does in its `review-pr-<n>` worktree, and prints the tier and the sensitive files.
//!
//! cargo run -p velocity-git --example review_tier -- <checkout> <base commit> <head commit>
use std::path::PathBuf;

use velocity_git::review_tier;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let checkout = PathBuf::from(arguments.next().ok_or("pass the checkout path")?);
    let base = arguments.next().ok_or("pass the base commit")?;
    let head = arguments.next().ok_or("pass the head commit")?;
    let assessment = review_tier(&checkout, &base, &head)?;
    println!("tier: {}", assessment.tier.label());
    for file in &assessment.sensitive_files {
        println!("sensitive: {file}");
    }
    Ok(())
}
