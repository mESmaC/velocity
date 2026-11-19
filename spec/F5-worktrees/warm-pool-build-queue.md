#### Warm pool (speed) · Build queue

<!-- split-spec: {"order":728,"kind":"markdown","indentation":0} -->
- **Build queue** (#480).
  Velocity queues agents' single-crate Rust type checks and Lean checks through Rosen's machine-wide queue.
  Agent light Rust runs (`cargo check -p <crate>` and the UI's `bindings` script) take one of two machine-wide slots, use two Cargo jobs at below-normal priority, and share one incremental target directory per project beside its workspaces (#486).
  A light run starts when 3 GiB fits above Rosen's memory reserve, measured rather than charged at other turns' caps, so an integration build no longer holds every agent check.
  Velocity puts shims for `cargo`, `rustc`, `cargo-clippy`, `cargo-tauri` and `clippy-driver` first on every agent's `PATH` (#483).
  Direct `cargo fmt` and `cargo check -p <crate>` take an agent light slot, and the shims refuse other direct invocations with the integration-build message shared by `rosen run` and `queue_run`.
  The managed paths also accept `cargo metadata --format-version 1` and `pnpm -C ui bindings` for generated-file repair.
  Agents use Rust formatting, UI type-check and lint as applicable.
  Warm pool prepares install UI dependencies and fetch the Lean cache.
  Integration builds use Rosen's `verify` class and wait for a memory-budgeted turn.
  The shared ACP client supplies the same command policy on launch and resume.
  `queue_run` takes an executable, separate arguments, an optional working directory and a Rosen class.
  Velocity starts it when the provider ends its turn, keeps the full output outside the workspace, and delivers the result and log path in the next prompt.
  Explicitly stopping the session cancels its queued commands.
  The managed foreground `rosen run` command applies the same Rust policy when the MCP tool is unavailable.
  New turns wait until their memory budget fits above Rosen's machine-wide reserve, which defaults to 4 GiB (#484).
  Velocity also checks headroom before starting each command within a check set (#481).
  A queued command counts as waiting in the build queue, so it is never stalled.

<!-- split-spec links -->

Rule files:

- [The run queue's classes overlap on purpose](warm-pool-build-queue-the-run-queue-s-classes-overlap.md)
- [Agents request GitHub workflow runs the owner allows](warm-pool-build-queue-workflow-runs.md)
