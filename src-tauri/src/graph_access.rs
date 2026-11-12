//! The code graph for every agent (F19): Velocity's own executable serves each agent's
//! `graph` MCP connection, started beside AIRC in every session (see `velocity-acp`'s
//! `graph_server`).

use anyhow::Context;
use velocity_graph::{SERVER_SWITCH, WARM_SWITCH, serve_stdio, warm_stdio};

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
        // The warm pool's warming run (F19 Warming): fill the parse cache and exit.
        Some(WARM_SWITCH) => warm_stdio(rest).context("Code graph warming failed")?,
        _ => return Ok(false),
    }
    Ok(true)
}
