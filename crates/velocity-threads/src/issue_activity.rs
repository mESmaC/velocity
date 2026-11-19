use serde::Deserialize;
use std::path::Path;
use velocity_airc::{Error, IssueUpdate};
use velocity_github::IssueClient;

#[derive(Deserialize)]
struct IssueProjects {
    projects: Vec<IssueProject>,
}

#[derive(Deserialize)]
struct IssueProject {
    id: u32,
    repository_path: String,
    github_remote: Option<String>,
}

/// The tool process reads the project's GitHub identity from the app, since a Rosen
/// workspace has no Git remote and the issue number alone is only repository-local.
pub fn post_issue_update(
    app_data: &Path,
    project: &str,
    update: IssueUpdate<'_>,
) -> Result<String, Error> {
    let invalid = |message: &str| Error::Invalid(message.into());
    let identifier = project
        .parse::<u32>()
        .map_err(|_| invalid("Invalid project identity"))?;
    // Startup recovery belongs to the app that owns the state writer. Read only
    // the atomically replaced snapshot from this MCP child process.
    let snapshot =
        velocity_store::extended_windows_path(&app_data.join(velocity_store::STATE_FILE_NAME));
    let file = std::fs::File::open(snapshot)?;
    let state: IssueProjects = serde_json::from_reader(std::io::BufReader::new(file))?;
    let project = state
        .projects
        .into_iter()
        .find(|project| project.id == identifier)
        .ok_or_else(|| invalid("The project was removed"))?;
    let directory = Path::new(&project.repository_path);
    let client = match &project.github_remote {
        Some(remote) => IssueClient::for_repository(directory, remote),
        None => IssueClient::discover(directory),
    }
    .map_err(|error| Error::Invalid(error.to_string()))?;
    let thread = update
        .claim
        .member
        .strip_prefix("thread-")
        .and_then(|member| member.parse::<u32>().ok());
    let byline = issue_byline(identifier, thread, Some(&update.claim.nick));
    if update.needs_owner {
        client.post_needs_owner(update.issue, &byline, update.body)
    } else {
        client.post_issue_progress(update.issue, &byline, update.body, update.ready)
    }
    .map_err(|error| Error::Invalid(error.to_string()))
}

pub fn issue_byline(project: u32, thread: Option<u32>, nick: Option<&str>) -> String {
    let agent = nick.map_or_else(
        || "Velocity agent".into(),
        |nick| format!("Velocity agent `{nick}`"),
    );
    match thread {
        Some(thread) => {
            let link = format!("velocity://thread/{project}/{thread}");
            format!("{agent} · [Thread {thread}]({link}) · `{link}`")
        }
        None => agent,
    }
}
