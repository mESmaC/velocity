#### Post-land verification

Velocity asks Rosen to land a named change on a clean merge with the base.
Rosen projects opened in Velocity use `review = "after-land"`.
Rosen status offers the one-line configuration change and a Switch to after-land action if the base still specifies `review = "before-land"` (#524).
The notice leaves the project usable, and unrelated named changes on a `before-land` base receive no automatic land request.
A named change sitting directly on the base that edits only `rosen.toml` to set `review = "after-land"` still reaches Rosen so a reviewed and receipted switch lands, and the status pane action proposes, approves on behalf of the owner, runs any configured pre-land checks, submits the receipt and lands that one-line switch.

A change that registers a Tauri command, event or constant without regenerating `ui/src/lib/bindings.ts` merges cleanly, so Velocity checks the bindings before it asks Rosen to land such a change (#503).
The check applies to a project that has `src-tauri/src/bin/export-bindings.rs`, and to a named change that edits `src-tauri/src/lib.rs`, the exporter, `bindings.ts` itself, a Rust file that names specta (`specta::Type`, `tauri_specta::Event`, `specta::specta`) or a Rust file it removes.
Velocity stages the change's merge onto the base in the project's `bindings-check` folder beside its workspaces and runs `cargo run --manifest-path src-tauri/Cargo.toml --bin export-bindings -- --check` there as a light run (F5 "Build queue").
The folder keeps its build between changes, and one check runs per project at a time.
The check runs off the landing path: other named changes keep landing, and a change stacked on the one being checked is tried again after the check instead of being refused.
When the exporter writes a different file, the land is refused as "`ui/src/lib/bindings.ts` is stale" and the change returns to its author with the regeneration command, like a regeneration refusal (#487).
A check that reaches no verdict, such as a merge result that does not compile, holds nothing: the feed says the bindings could not be checked and the change lands for the integration build to judge.
A merge with conflicts or generated files to regenerate is not checked, because Rosen refuses that land itself.

Rosen R12 owns the queue of verification jobs, bisection, confirmed failures and automatic reverts under its configured policy.
Velocity runs Rosen's commands on the requested commit in a reusable build folder and reports their results.
The worker runs independently of Scouts and the review swarm.
Velocity's batched integration build runs compilation, lint and full tests, and a confirmed broken build can start a fix-forward thread.
Velocity stops an integration command and its child processes after 10 minutes without new log output and reports the failure with the log tail (#486).
A revert returns the change and failing output to its author.

Velocity can close a source issue 30 minutes after verified R12 build success when the mirror carries the landed commit and the exact land has no review claim or objection.
A positive review swarm verdict takes precedence, and a revert reopens the issue with the culprit output.
The Close issues option governs issue swarms.
