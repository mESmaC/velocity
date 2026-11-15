//! Read-only dispatch preview, including GitHub reads from a Rosen workspace without Git.
//! `rosen run -- cargo run -p velocity-threads --example continuous_dispatch -- <folder> <owner/repo>`
use velocity_github::IssueClient;
use velocity_store::ProjectItem;
use velocity_threads::continuous_dispatch::{DispatchView, ordered_issues};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let folder = arguments.next().ok_or("Pass a project folder")?;
    let repository = arguments.next().ok_or("Pass owner/repo")?;
    let client = IssueClient::for_repository(std::path::Path::new(&folder), &repository)?;
    let mut project = ProjectItem::new(1, repository, folder);
    project.spec_locked = true;
    project.continuous_dispatch.slots = 3;
    let issues = client.open_issues()?;
    let ready = ordered_issues(&project, client.repository(), &issues, &[]);
    println!("{} open issues, {} available", issues.len(), ready.len());
    println!(
        "{}",
        serde_json::to_string_pretty(&DispatchView::new(&project, &ready, None))?
    );
    Ok(())
}
