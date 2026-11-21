#### Rosen · What the agent is told

<!-- split-spec: {"order":557,"kind":"markdown","indentation":0} -->
- **What the agent is told.**
  The agent's process gets `ROSEN_ENDPOINT`, `ROSEN_AGENT`, `ROSEN_CHANGE` and Rosen's folder on `PATH`, so the `rosen` CLI reaches the project's server as that agent, on its change.
  Its session also gets Rosen's own MCP server, `rosen mcp --workspace <folder>` (Rosen R10), which offers the same commands as tools; it is left out when the `rosen` program isn't found.
  Its fresh session's first prompt starts with one short session card (not shown in the transcript and never re-injected on a loaded session) holding only per-thread values: identity and workspace, the Rosen change, base tip, held claims, overlapping changes, machine queue depth and any unavailable tools (#534).
  Static workflow, build-queue, `cargo` shim, GitHub and browser rules live once in `AGENTS.md`, with one check rule: always use `velocity-run`'s `queue_run` (and `preflight` before naming the change), falling back to `rosen run -- <command>` only when `velocity-run` is listed under `Unavailable tools` (#534).
  Adjacent text blocks in the preface and opening prompt are joined with a blank line so no ACP adapter concatenates them without whitespace (#534).
