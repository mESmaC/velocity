//! Read a real PR into the same observer the native app uses; never changes it.
use std::path::Path;
use velocity_github::{PullRequestClient, follow_through::FollowThrough};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().collect();
    let directory = arguments.get(1).ok_or("expected repository directory")?;
    let number = arguments.get(2).ok_or("expected PR number")?.parse()?;
    let snapshot = PullRequestClient::discover(Path::new(directory))?.snapshot(number)?;
    let mut observer = FollowThrough::default();
    observer.observe(&snapshot);
    println!("{}", serde_json::to_string_pretty(&observer)?);
    let original = observer.clone();
    observer.observe(&snapshot);
    if observer != original {
        return Err("an unchanged observation was not idempotent".into());
    }
    Ok(())
}
