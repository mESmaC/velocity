//! Read a real PR into the same observer the native app uses; never changes it.
//!
//! `observe_pull_request <repository> <number> [<worktree at the PR head>]`: with a worktree,
//! also runs the proof checks there and records the proof card (F17), as the app's watcher
//! does in the thread's worktree, then prints the PR track.
use std::path::Path;
use velocity_github::{PullRequestClient, follow_through::FollowThrough};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().collect();
    let directory = arguments.get(1).ok_or("expected repository directory")?;
    let number = arguments.get(2).ok_or("expected PR number")?.parse()?;
    let snapshot = PullRequestClient::discover(Path::new(directory))?.snapshot(number)?;
    let mut observer = FollowThrough::default();
    if !observer.should_follow(&snapshot.pull_request) {
        println!("Draft PR #{number} is still a claim; the thread keeps coding until it is ready.");
        return Ok(());
    }
    observer.observe(&snapshot);
    println!("{}", serde_json::to_string_pretty(&observer)?);
    let original = observer.clone();
    observer.observe(&snapshot);
    if observer != original {
        return Err("an unchanged observation was not idempotent".into());
    }
    if let Some(worktree) = arguments.get(3).map(Path::new) {
        let base = velocity_proofs::base_reference(worktree, &snapshot.pull_request.base_ref_name);
        let head = observer.head.clone();
        let card = velocity_proofs::proof_card(worktree, &base, &head, None)?;
        observer.record_proofs(&head, card);
        let view = observer.view();
        let track: Vec<String> = view
            .track
            .iter()
            .map(|step| format!("{} [{}]", step.label, step.state))
            .collect();
        println!("track: {}", track.join(" → "));
        match &view.proof {
            Some(card) => println!("proved: {} · {}", view.proved, card.summary()),
            None => println!("no proof card"),
        }
    }
    Ok(())
}
