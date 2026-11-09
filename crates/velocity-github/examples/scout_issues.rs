//! Backend entry point: inspect all issues, or execute a scout finding from JSON.
//! `scout_issues <checkout> [--prepare|--file <finding.json>]`.
use std::path::Path;

use velocity_github::{IssueClient, ScoutFinding};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let directory = Path::new(arguments.first().map(String::as_str).unwrap_or("."));
    let client = IssueClient::discover(directory)?;
    let output = match arguments.get(1).map(String::as_str) {
        None => serde_json::to_value(client.inventory()?)?,
        Some(operation @ ("--prepare" | "--file")) => {
            let path = arguments.get(2).ok_or("Provide a finding JSON file")?;
            let finding: ScoutFinding = serde_json::from_slice(&std::fs::read(path)?)?;
            serde_json::to_value(client.file_scout_finding(&finding, operation == "--file")?)?
        }
        Some(_) => return Err("Use <checkout> [--prepare|--file <finding.json>]".into()),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
