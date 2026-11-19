//! Read-only dispatch preview, including GitHub reads from a Rosen workspace without Git.
//! `rosen run -- cargo run -p velocity-threads --example continuous_dispatch -- <folder> <owner/repo> [<owner/repo>…]`
//! Further repositories stand in for the owner's other projects, whose issues can wait on
//! the first repository's (#510).
use velocity_github::{DependencyGraph, IssueClient};
use velocity_store::ProjectItem;
use velocity_threads::continuous_dispatch::{DispatchView, ordered_work};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let folder = arguments.next().ok_or("Pass a project folder")?;
    let repository = arguments.next().ok_or("Pass owner/repo")?;
    let client = IssueClient::for_repository(std::path::Path::new(&folder), &repository)?;
    let mut project = ProjectItem::new(1, repository, folder.clone());
    project.spec_locked = true;
    project.continuous_dispatch.slots = 3;
    let issues = client.open_issues()?;
    let mut dependencies = DependencyGraph::default();
    dependencies.replace_source(project.id, client.repository(), &issues);
    for (source, other) in (project.id + 1..).zip(arguments) {
        let other = IssueClient::for_repository(std::path::Path::new(&folder), &other)?;
        dependencies.replace_source(source, other.repository(), &other.open_issues()?);
    }
    let ready = ordered_work(
        &project,
        client.repository(),
        &issues,
        &Default::default(),
        true,
        &dependencies.unblock_counts(client.repository()),
    );
    println!(
        "{} open issues, {} available items",
        issues.len(),
        ready.len()
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&DispatchView::new(&project, &ready, None))?
    );
    Ok(())
}
