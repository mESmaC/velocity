//! The code graph for every agent (F19): Velocity's own executable serves each agent's
//! `graph` MCP connection, started beside AIRC in every session (see `velocity-acp`'s
//! `graph_server`).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use velocity_graph::{
    PROJECT_SWITCH, SERVER_SWITCH, WARM_SWITCH, serve_project, serve_stdio, warm_stdio,
};
use velocity_rosen::ProjectServer;
use velocity_store::{ProjectItem, VersionControl};

/// `velocity --graph-mcp <app-data> <project-id> <worktree>`: serve one agent's graph tools on
/// stdin/stdout and exit. `false` when Velocity was started normally.
pub fn serve_graph_server() -> anyhow::Result<bool> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let Some((switch, rest)) = arguments.split_first() else {
        return Ok(false);
    };
    match switch.to_str() {
        Some(SERVER_SWITCH) => {
            serve_stdio(rest).context("The code graph MCP connection failed")?;
        }
        Some(PROJECT_SWITCH) => serve_project(rest).context("The project graph worker failed")?,
        // The warm pool's warming run (F19 Warming): fill the parse cache and exit.
        Some(WARM_SWITCH) => warm_stdio(rest).context("Code graph warming failed")?,
        _ => return Ok(false),
    }
    Ok(true)
}

pub(crate) fn issue_paths(
    data: &Path,
    project: &ProjectItem,
    texts: &[(&str, &str)],
) -> Vec<BTreeSet<String>> {
    let Ok(executable) = std::env::current_exe() else {
        return vec![BTreeSet::new(); texts.len()];
    };
    let endpoint = (project.version_control == VersionControl::Rosen)
        .then(|| ProjectServer::new(data, project.id).endpoint().clone());
    velocity_graph::mentioned_paths(
        &executable,
        data,
        &project.id.to_string(),
        Path::new(&project.repository_path),
        endpoint,
        texts,
    )
}
