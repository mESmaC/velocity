#### Rosen · What the agent is told

<!-- split-spec: {"order":557,"kind":"markdown","indentation":0} -->
- **What the agent is told.**
  The agent's process gets `ROSEN_ENDPOINT`, `ROSEN_AGENT`, `ROSEN_CHANGE` and Rosen's folder on `PATH`, so the `rosen` CLI reaches the project's server as that agent, on its change.
  Its session also gets Rosen's own MCP server, `rosen mcp --workspace <folder>` (Rosen R10), which offers the same commands as tools; it is left out when the `rosen` program isn't found.
  Its first prompt, and a resumed session's, starts with a note that is not shown in the transcript: the folder is a Rosen workspace, not a git checkout, so git branch, commit, push and PR steps don't apply; `rosen commit -m` names the work.
  The note tells the agent to run `velocity-run`'s `preflight` before naming the change, use `queue_run` for other commands that can take more than a minute, end the turn, and continue from Velocity's result.
  Shorter builds and tests use `rosen run -- <command>`, as do commands that need the queue when the tool is unavailable.
