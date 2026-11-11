//! Entry point for inspecting PRs or executing a typed operation from a JSON file.
//! `--operation <json> [<app-data> <project-id> <nick>]` attaches the AIRC
//! approval record that approve, request changes and merge use (F7).
use std::path::Path;
use velocity_github::{
    PullRequestClient, PullRequestOperation, PullRequestPanelCache, ReviewRecord,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let directory = arguments.first().map(String::as_str).unwrap_or(".");
    let mut client = PullRequestClient::discover(Path::new(directory))?;
    if let [_, _, _, app_data, project, nick] = arguments.as_slice() {
        client = client.with_review_record(ReviewRecord::open(Path::new(app_data), project, nick)?);
    }
    let output = if arguments
        .get(1)
        .is_some_and(|argument| argument == "--operation")
    {
        let path = arguments.get(2).ok_or("--operation requires a JSON file")?;
        let operation: PullRequestOperation = serde_json::from_slice(&std::fs::read(path)?)?;
        serde_json::to_value(client.execute(operation)?)?
    } else if arguments
        .get(1)
        .is_some_and(|argument| argument == "--panel")
    {
        // Tiers come from the checkout's AGENTS.md, as on the rail (F7).
        let agents =
            std::fs::read_to_string(Path::new(directory).join("AGENTS.md")).unwrap_or_default();
        let mut tiers = PullRequestPanelCache::default();
        serde_json::to_value(client.tiered_panel(&mut tiers, &agents)?)?
    } else if let Some(number) = arguments.get(1) {
        serde_json::to_value(client.snapshot(number.parse()?)?)?
    } else {
        serde_json::to_value(client.list()?)?
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
