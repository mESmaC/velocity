#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // Sweeps start Velocity's own executable as each scout's MCP server (F7).
    if velocity_app_lib::sweeps::serve_scout_server()? {
        return Ok(());
    }
    // Every agent session starts Velocity's own executable as its AIRC MCP server (F14).
    if velocity_app_lib::airc_access::serve_airc_server()? {
        return Ok(());
    }
    velocity_app_lib::run()
}
