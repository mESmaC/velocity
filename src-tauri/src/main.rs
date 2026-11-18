#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod queued_run_access;

fn main() -> anyhow::Result<()> {
    if let Some(result) = velocity_rosen::agent_run::serve() {
        let code = match result {
            Ok(status) => status.code().unwrap_or(1),
            Err(error) => {
                eprintln!("rosen: {error}");
                1
            }
        };
        std::process::exit(code);
    }
    if queued_run_access::serve()? {
        return Ok(());
    }
    if let Ok(executable) = std::env::current_exe() {
        velocity_acp::client::enable_queued_runs(executable);
    }
    if velocity_app_lib::headless::serve()? {
        return Ok(());
    }
    // Sweeps start Velocity's own executable as each scout's MCP server (F7).
    if velocity_app_lib::sweeps::serve_scout_server()? {
        return Ok(());
    }
    // Reviews start it as each PR duty agent's MCP server (F7).
    if velocity_app_lib::reviews::serve_pr_duty_server()? {
        return Ok(());
    }
    // And as the MCP server of each PR duty agent reviewing a Rosen change (F5, F7).
    if velocity_app_lib::reviews::serve_rosen_duty_server()? {
        return Ok(());
    }
    // Watches start it as the Main watcher's MCP server (F7).
    if velocity_app_lib::main_watch::serve_watch_server()? {
        return Ok(());
    }
    // Every agent session starts Velocity's own executable as its AIRC MCP server (F14).
    if velocity_app_lib::airc_access::serve_airc_server()? {
        return Ok(());
    }
    // Every agent session also starts it as its code graph MCP server (F19).
    if velocity_app_lib::graph_access::serve_graph_server()? {
        return Ok(());
    }
    if velocity_updates::supervision::supervise()? {
        return Ok(());
    }
    velocity_rosen::agent_run::install(&std::env::current_exe()?);
    velocity_app_lib::run()
}
