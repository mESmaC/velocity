//! Run the Rosen issue follow-up transport without a window. Used with a fake gh for
//! a smoke run; normal calls use the same OAuth transport as the application's follower.
use std::path::Path;
use velocity_github::{IssueClient, RosenIssueEffect};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let [directory, number, commit, action, reason] = arguments.as_slice() else {
        return Err(
            "expected: <repository folder> <issue> <landed SHA> <close|reopen> <reason>".into(),
        );
    };
    let reopen = match action.as_str() {
        "close" => false,
        "reopen" => true,
        _ => return Err("expected close or reopen".into()),
    };
    let client = IssueClient::discover(Path::new(directory))?;
    let done = client.follow_rosen_issue(
        client.repository(),
        number.parse()?,
        commit,
        if reopen { 2 } else { 1 },
        if reopen {
            RosenIssueEffect::Reverted { reason }
        } else {
            RosenIssueEffect::Watcher
        },
    )?;
    if !done {
        println!("Pending: the mirror has not carried the land yet");
        return Ok(());
    }
    println!("Issue #{number}: {action} complete");
    Ok(())
}
