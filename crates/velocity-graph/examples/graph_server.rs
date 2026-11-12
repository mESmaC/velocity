//! The graph MCP server on its own, for headless runs: `graph_server <app-data> <project-id> <worktree>`.
fn main() -> Result<(), velocity_graph::Error> {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    velocity_graph::serve_stdio(&arguments)
}
