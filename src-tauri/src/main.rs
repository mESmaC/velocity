#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // Sweeps start Velocity's own executable as each scout's MCP server (F7).
    if velocity_app_lib::sweeps::serve_scout_server()? {
        return Ok(());
    }
    velocity_app_lib::run()
}
