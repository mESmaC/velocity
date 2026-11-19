//! Builds a local release of one of Velocity's own landed commits (SPEC.md F1 "Local
//! builds", #512). The commit is staged in a folder of its own beside the project's
//! post-land checks, so the release `target` and the UI's dependencies are reused from one
//! build to the next, and `scripts/release-local.ps1` runs there as one background turn in
//! the machine's run queue: at low priority, behind landing checks and agents' own builds.

use std::path::{Path, PathBuf};

use velocity_rosen::checker::{self, FailedCommand, Priority};
use velocity_rosen::{ProjectServer, post_land};
use velocity_store::duty::LandingProject;

use crate::launch::Location;

/// The project's folder the releases build in, beside its agents' workspaces.
const BUILD_FOLDER: &str = "local-release";
const SCRIPT: &str = "scripts/release-local.ps1";
/// How much of a failed command's output the reason keeps.
const REASON_LINES: usize = 12;

/// One build: the landed commit, the version it is given and where its downloads go.
pub struct LocalRelease {
    pub commit: String,
    pub version: String,
    pub out_dir: PathBuf,
}

/// Stages `release.commit` and builds it. An error is the reason to show: the commit could
/// not be staged, it has no release script, or a command failed.
pub async fn build(
    app_data: &Path,
    project: &LandingProject,
    release: &LocalRelease,
) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Local builds are made on Windows only".into());
    }
    let folder = Location::new(&project.project, &project.worktrees)
        .project_root()
        .map_err(|error| error.to_string())?
        .join(BUILD_FOLDER);
    let server = ProjectServer::new(app_data, project.project.id);
    post_land::stage_commit(&server, &release.commit, &folder)
        .await
        .map_err(|error| error.to_string())?;
    let commands = commands(release);
    tokio::task::spawn_blocking(move || run(&folder, &commands))
        .await
        .map_err(|error| error.to_string())?
}

/// The staged tree has no `ui/node_modules` the first time, and may need new ones later.
fn commands(release: &LocalRelease) -> Vec<String> {
    let out_dir = release.out_dir.to_string_lossy();
    // A trailing backslash would escape the closing quote.
    let out_dir = out_dir.trim_end_matches('\\');
    vec![
        "pnpm -C ui install --frozen-lockfile".to_owned(),
        format!(
            "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts\\release-local.ps1 -Version {} -OutDir \"{out_dir}\"",
            release.version
        ),
    ]
}

fn run(folder: &Path, commands: &[String]) -> Result<(), String> {
    checker::apply_staged(folder).map_err(|error| error.to_string())?;
    if !folder.join(SCRIPT).is_file() {
        return Err(format!("This commit has no {SCRIPT}"));
    }
    match checker::run_commands(folder, commands, Priority::Background) {
        Ok(None) => Ok(()),
        Ok(Some(failed)) => Err(reason(&failed)),
        Err(error) => Err(error.to_string()),
    }
}

/// The command, how it ended and the end of what it printed.
fn reason(failed: &FailedCommand) -> String {
    let ended = failed
        .exit_code
        .map_or_else(|| "timed out".to_owned(), |code| format!("exited {code}"));
    let lines: Vec<&str> = failed
        .output
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(REASON_LINES)..].join("\n");
    format!("`{}` {ended}\n{tail}", failed.command)
        .trim_end()
        .to_owned()
}
