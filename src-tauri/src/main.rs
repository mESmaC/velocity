#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod queued_run_access;

/// Rosen's binaries ride inside a release (F5). They are a `static` in the executable's
/// own crate, so no library's metadata carries them and no other crate rebuilds for them.
#[cfg(feature = "bundled-rosen")]
static ROSEN_BINARIES: [(&str, &[u8]); 2] = [
    (
        "rosen-server.exe",
        include_bytes!(env!("VELOCITY_ROSEN_SERVER_EXE")),
    ),
    ("rosen.exe", include_bytes!(env!("VELOCITY_ROSEN_CLI_EXE"))),
];

fn main() -> anyhow::Result<()> {
    velocity_rosen::mcp_latency::mark_process_start();
    if let Some(app_data) = helper_app_data() {
        velocity_rosen::mcp_latency::set_default_app_data(app_data);
    }
    // The release builds Rosen from the commit the workspace manifest pins, so that names
    // the binaries: a later Velocity shows it for a server still running this build (#515).
    #[cfg(feature = "bundled-rosen")]
    velocity_rosen::bundled::provide(
        &ROSEN_BINARIES,
        velocity_core::rosen_pin::RosenPin::parse(include_str!("../../Cargo.toml"))
            .map(|pin| pin.revision),
    );
    if let Some(code) = velocity_acp::providers::serve_codex_app_server()? {
        std::process::exit(code);
    }
    if let Some(argument) = std::env::args_os()
        .nth(1)
        .and_then(|value| value.into_string().ok())
    {
        velocity_app_lib::process_memory::start_helper_guard(&argument);
        if argument == velocity_browser::SWITCH {
            return Ok(velocity_browser::serve_mcp()?);
        }
    }
    if let Some((command, result)) = velocity_rosen::agent_run::serve() {
        let code = match result {
            Ok(status) => status.code().unwrap_or(1),
            Err(error) => {
                eprintln!("{command}: {error}");
                1
            }
        };
        std::process::exit(code);
    }
    if queued_run_access::serve()? {
        return Ok(());
    }
    if let Ok(executable) = std::env::current_exe() {
        velocity_acp::providers::enable_codex_startup_override(executable.clone());
        velocity_acp::client::enable_queued_runs(executable);
    }
    if velocity_app_lib::headless::serve()? {
        return Ok(());
    }
    // Sweeps start Velocity's own executable as each scout's MCP server (F7).
    if velocity_app_lib::sweeps::serve_scout_server()? {
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

fn helper_app_data() -> Option<std::path::PathBuf> {
    #[derive(serde::Deserialize)]
    struct AppConfig {
        identifier: String,
    }
    let config: AppConfig = serde_json::from_str(include_str!("../tauri.conf.json")).ok()?;
    Some(dirs::data_dir()?.join(config.identifier))
}
