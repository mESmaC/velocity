//! Velocity's `rosen` MCP proxy entrypoint (`--rosen-mcp`), wrapping `rosen mcp` with a
//! non-blocking parking `wait` tool and aligned `rosen claim --exclusive <paths>` guidance (#531).

pub fn serve() -> anyhow::Result<bool> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments
        .next()
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
        != Some(velocity_rosen::mcp_proxy::SERVER_SWITCH)
    {
        return Ok(false);
    }
    let rosen_args: Vec<String> = arguments
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| anyhow::anyhow!("Rosen MCP arguments must be valid UTF-8"))
        })
        .collect::<Result<_, _>>()?;
    velocity_rosen::mcp_proxy::serve_stdio(&rosen_args)?;
    Ok(true)
}
