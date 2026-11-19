//! Read GitHub pull requests that are still referenced by legacy threads.

use std::path::Path;
use velocity_github::PullRequestClient;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let directory = args.next().ok_or("Expected repository path")?;
    let client = PullRequestClient::discover(Path::new(&directory))?;
    let value = match args.next() {
        Some(number) => serde_json::to_value(client.snapshot(number.parse()?)?)?,
        None => serde_json::to_value(client.list()?)?,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
