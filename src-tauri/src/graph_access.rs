//! The code graph for every agent (F19): Velocity's own executable serves each agent's
//! `graph` MCP connection, started beside AIRC in every session (see `velocity-acp`'s
//! `graph_server`).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context;
use velocity_graph::{
    Graph, Index, PROJECT_SWITCH, SERVER_SWITCH, WARM_SWITCH, serve_project, serve_stdio,
    warm_stdio,
};
use velocity_rosen::{ProjectServer, post_land::BaseSnapshot};
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

#[derive(Default)]
pub(crate) struct IssueGraph {
    index: Option<Index>,
    revision: Option<String>,
    checked: Option<Instant>,
}

impl IssueGraph {
    /// Called by background filing and dispatch. Reuse the shared parse cache and only
    /// rebuild a Rosen graph when its base moves.
    pub(crate) fn get(&mut self, data: &Path, project: &ProjectItem) -> Option<&Graph> {
        if self
            .checked
            .is_none_or(|time| time.elapsed() >= Duration::from_secs(30))
        {
            self.checked = Some(Instant::now());
            self.refresh(data, project);
        }
        self.index.as_ref().map(|index| &index.graph)
    }

    fn refresh(&mut self, data: &Path, project: &ProjectItem) {
        let cache = data.join("graph").join(project.id.to_string());
        if project.version_control != VersionControl::Rosen {
            self.index = Index::open(Path::new(&project.repository_path), &cache).ok();
            return;
        }
        let server = ProjectServer::new(data, project.id);
        let Ok(base) = tauri::async_runtime::block_on(BaseSnapshot::read(&server)) else {
            return;
        };
        if self.revision.as_deref() == Some(base.commit()) {
            return;
        }
        if let Ok(index) = Index::open_revision(server.endpoint().clone(), base.commit(), &cache) {
            self.revision = Some(base.commit().to_owned());
            self.index = Some(index);
        }
    }
}
