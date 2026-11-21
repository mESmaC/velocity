#### Warm pool (speed) · Build queue

<!-- split-spec: {"order":728,"kind":"markdown","indentation":0} -->
- **Build queue** (#480).
  Velocity queues agents' single-crate Rust type checks and Lean checks through Rosen's machine-wide queue.
  Agent light Rust runs (`cargo check -p <crate>` and the UI's `bindings` script) take one of two machine-wide slots, use two Cargo jobs at below-normal priority, and share one incremental target directory per project beside its workspaces (#486).
  When a light run switches to a different workspace (or a recreated Rosen change at the same path), Velocity invalidates local workspace crate fingerprints and build-script outputs in the shared target directory before Cargo runs while keeping registry and git dependency artifacts warm (#527).
  Velocity's own bindings check before a land (F7 "Post-land verification", #503) is a light run too, in the same slots and target directory.
  A light run starts when 3 GiB fits above Rosen's memory reserve, measured rather than charged at other turns' caps, so an integration build no longer holds every agent check.
  Velocity puts shims for `cargo`, `rustc`, `cargo-clippy`, `cargo-tauri` and `clippy-driver` first on every agent's `PATH` (#483).
  Direct `cargo fmt` and `cargo check -p <crate>` take an agent light slot, and the shims refuse other direct invocations with the integration-build message shared by `rosen run` and `queue_run`.
  A shim reached inside a turn already held (`ROSEN_RUN_HELD` set, as in a queued light run) runs the real tool at once, found outside the shim folders, and never waits for a turn of its own: that turn's holder is its own ancestor (#548).
  Queued light, utility and `lake` runs keep Velocity's shim folder off the child `PATH` so they invoke the real toolchain directly, and direct shim lock or headroom waits time out after 20 minutes with a clear error instead of blocking forever (#548).
  The managed paths (including a `cargo` shim reached inside a held turn) also accept `cargo metadata --format-version 1`, `pnpm -C ui bindings` and `cargo run ... --bin export-bindings` for generated-file repair (#559).
  Agents use Rust formatting, UI type-check and lint as applicable.
  Warm pool prepares install UI dependencies and fetch the Lean cache.
  Integration builds use Rosen's `verify` class and wait for a memory-budgeted turn.
  The shared ACP client supplies the same command policy on launch and resume.
  `queue_run` takes an executable, separate arguments, an optional working directory and a Rosen class, and returns a durable run ID, queue position and estimated wait (#536).
  Velocity starts it when the provider ends its turn, keeps the full output outside the workspace, and delivers the result and log path in the next prompt.
  `velocity-run` also exposes `run_status` (reporting a run's phase, queueing reason, checked Rosen change and head, exit status, log excerpt and log path, or listing recent runs to recover missed results) and `run_cancel` (cancelling a pending or running command by run ID) (#536).
  Explicitly stopping the session cancels its queued commands.
  Outstanding queued runs (durable ID, command, working folder, class, last phase, queue reason, log path, and checked change/head) and recent finished run history are persisted on their owning thread in `state.json` (#516, #536).
  After a Velocity restart, Velocity requeues interrupted runs and delivers a restart notice listing each interrupted run's ID, command, last phase and log path together with any results recorded before shutdown (or stating explicitly when there were none); if starting any interrupted run is impossible, the notice names the lost runs and the reason (#516, #536).
  A queued run that times out waiting for a run-queue turn (after 20 minutes) is reported to the thread with the wait reason, current queue line and memory readings, and is never silently retried.
  The managed foreground `rosen run` command applies the same Rust policy when the MCP tool is unavailable.
  New turns wait until their memory budget fits above Rosen's machine-wide reserve, which defaults to 4 GiB (#484).
  Velocity also checks headroom before starting each command within a check set (#481).
  A queued command counts as waiting in the build queue, so it is never stalled.
  `velocity-run` also offers `preflight` (#537), which agents call before naming a change.
  After the turn ends, Velocity snapshots the workspace, reads the change's diff and the base's `rosen.toml`, and runs only the checks that apply: `rustfmt --check` on changed Rust files only, UI type-check and lint when `ui/` changed, queued `cargo check -p` for each touched crate plus `cargo clippy -p` for touched crates on sensitive paths, active overlaps with symbol analysis, and generated-file freshness checks (`Cargo.lock`, `ui/pnpm-lock.yaml`, `ui/src/lib/bindings.ts` and Rosen merge regeneration).
  Velocity keeps the full output outside the workspace and delivers one pass/fail report with excerpts from any failing step in the agent's next prompt.

<!-- split-spec links -->

Rule files:

- [The run queue's classes overlap on purpose](warm-pool-build-queue-the-run-queue-s-classes-overlap.md)
- [Agents request GitHub workflow runs the owner allows](warm-pool-build-queue-workflow-runs.md)
