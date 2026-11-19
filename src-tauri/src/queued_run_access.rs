use std::io;

use serde_json::Value;
use velocity_acp::client::{
    QUEUED_RUN_SERVER, QUEUED_RUN_SWITCH, QUEUED_RUN_TOKEN_ENV, QUEUED_RUN_TOOL, WORKFLOW_RUN_TOOL,
    queued_run_tool, request_queued_run, request_workflow_run, workflow_run_tool,
};
use velocity_github::mcp_stdio::{self, ToolServer};

pub fn serve() -> anyhow::Result<bool> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments
        .next()
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
        != Some(QUEUED_RUN_SWITCH)
    {
        return Ok(false);
    }
    let port = arguments
        .next()
        .and_then(|port| port.into_string().ok())
        .ok_or_else(|| anyhow::anyhow!("Missing queued run port"))?
        .parse()?;
    let token = std::env::var(QUEUED_RUN_TOKEN_ENV)?;
    mcp_stdio::serve(
        &mut Server { port, token },
        io::stdin().lock(),
        io::stdout().lock(),
    )?;
    Ok(true)
}

struct Server {
    port: u16,
    token: String,
}

impl ToolServer for Server {
    fn name(&self) -> &'static str {
        QUEUED_RUN_SERVER
    }

    fn instructions(&self) -> String {
        "Queue a long command, or request a GitHub workflow run the project owner allows,          then end your turn to receive its result from Velocity."
            .into()
    }

    fn tools(&self) -> Vec<Value> {
        vec![queued_run_tool(), workflow_run_tool()]
    }

    fn call(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String> {
        match name {
            Some(QUEUED_RUN_TOOL) => request_queued_run(self.port, &self.token, arguments),
            Some(WORKFLOW_RUN_TOOL) => request_workflow_run(self.port, &self.token, arguments),
            _ => Err("Unknown queued run tool".into()),
        }
    }
}
