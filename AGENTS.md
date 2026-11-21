# AGENTS.md · Velocity

Velocity is a Rust + Tauri desktop IDE for running coding agents in parallel.

## Scope

- Follow the issue, §7 item or named spec section for your work. Find the applicable headings with `graph_find` (kind: section) and read them with `graph_read` (source: true). If the graph is unavailable or clips a section, read just that section's lines. Use the project instructions already in your session and keep spec reads to the sections you need.
- SPEC.md §1 governs product rules. Agents run unrestricted with all tools and no approval gates. Providers sign in through OAuth. The frontend calls Tauri commands and subscribes to events. It never runs shell commands.
- Take the first unclaimed, unblocked item in the lowest milestone when choosing work (or call AIRC `next_work` to claim it atomically with its task brief). Use §7 for build order and §8 for open decisions that affect your piece. Skip blocked items. Treat *(proposed)* decisions as ready to build. File a `spec-question` issue with the recommended answer and blocked items when a decision is missing. File contradictions and unrelated findings as issues. Edit only your claimed piece.
- Follow the AIRC Task List instructions. Continuous dispatch can maintain the list. Other threads change it only at the user's request.

## Rosen workspace

- Use Rosen terms: workspace, change, snapshot, name the change, land, base, stored conflict, land history and path claim. A Rosen workspace has `.rosen`. Work in the workspace Velocity assigned and leave its cleanup to Velocity.
- Claim every intended path before editing with `rosen claim <paths>`. The first claim wins. Use `rosen share <paths>` for deliberate overlap, `rosen wait <paths>` for a dependency and `rosen release <paths>` to give up a claim.
- Follow the workspace note for issue follow-through. Where the thread owns issue updates, use AIRC `issue_comment` for start, a changed plan or a blocker, and `issue_ready_to_close` for verified completion. Leave the issue open for Velocity.
- Run and verify working functionality before proofs, tests or docs. Use `rosen status` and `rosen diff` to inspect stored work. Name the completed change with `rosen commit -m "<summary> (#issue)"` using the assigned issue or §7 item, then end your turn. Never approve a review, submit receipts or land your own change. Never use git or gh in the Rosen workspace.
- With `review = "after-land"`, Velocity lands a clean named change. The integration build verifies landed changes in batches. Fix a confirmed failure in the follow-up thread Velocity starts. For a stored conflict, run `rosen rebase`, combine both sides, run `rosen resolve <file>` for each conflict and name the change again.
- Address every review comment with a fix or a reason. Check fixes and name the change again.

## Shared files

- Velocity ticks the owning §7 item automatically when a Rosen change lands, so agents do not edit SPEC.md §7 in every change. Register commands, events, modules, members and dependencies in the appropriate group and place new named groups alphabetically. Use the existing implementation if the base lands the same fix first.
- When Rosen reports `regenerate`, regenerate `Cargo.lock` with `rosen run -- cargo metadata --format-version 1`, `ui/pnpm-lock.yaml` with `pnpm -C ui install --lockfile-only` and `ui/src/lib/bindings.ts` by queuing `pnpm -C ui bindings` or using `rosen run -- pnpm -C ui bindings`. Regenerate from the merged inputs and name the change again. Keep generated artifacts and registration lists in their respective `rosen.toml` lists.

## Commands

- Run `preflight` from `velocity-run` before naming the change, then end your turn and continue from the pass/fail report Velocity sends. It inspects the change's diff and runs only the checks that apply (`rustfmt --check` on changed Rust files, UI type-check and lint when `ui/` changed, queued `cargo check -p` for touched crates plus `cargo clippy -p` for touched crates on sensitive paths, active overlaps and generated-file freshness).
- Queue targeted `cargo check -p <crate>` or Lean checks during development with `queue_run` from `velocity-run`, or use `rosen run -- cargo check -p <crate>`. End your turn after an MCP run queues and continue from the result Velocity sends.
- Format only changed Rust files (`rustfmt --edition 2024 <files>`) rather than `cargo fmt --all`, which can rewrite unclaimed files. For UI formatting use `pnpm -C ui format`. The integration build runs full Rust compilation, clippy and tests; the release gate runs stress.

## Code and UI

- `src-tauri/` handles Tauri commands and events. Put logic in domain crates. `velocity-core` is pure state and decisions, `velocity-store` persists them, `velocity-threads` starts and runs threads, and `velocity-acp` handles agent sessions. Use the crate manifests for the rest of the workspace layout.
- Use pinned stable Rust, edition 2024. Use `thiserror` in crates and `anyhow` in Tauri. No `unsafe`, and no `unwrap()` or `expect()` outside `stress/`. Keep functions under 60 lines, use descriptive names without abbreviations and explain unclear decisions in comments.
- Handle Windows drive letters, spaces, backslashes and long paths. Use the workspace's pinned `rosen-client` and `rosen-core` together. Use constructors across modules and add `#[serde(default)]` to new persisted fields. Keep algorithms that need proofs pure and synchronous.
- Use the IPC names in §3.1. Document new commands and events in their feature's `spec/3.1-ipc/` file, one entry each. Place UI wrappers and re-exports in `ui/src/lib/ipc/<feature>.ts` and export from `ipc.ts`. Only IPC wrappers call `invoke` or `listen`. Generate command and event types with tauri-specta.
- Use Svelte 5 runes, plain Vite and strict TypeScript. No SvelteKit, Svelte 4 stores, `export let`, `any` or TypeScript bypass directives. Keep state changes in Rust.
- Open `design/main-window.html` in a browser for UI work. Match its layout, states and §5 keyboard map. Edit the reference only with user authorization. Keep its `:root` copied exactly in `ui/src/lib/theme/tokens.css`; add missing values to `reference.css` from the reference's winning declarations. Use `var(--token)` for visuals. Use smoked glass on black (`#000`), with no Mica, vibrancy or italics.

## Implementation first

- Working implementation outweighs proofs, tests and docs combined. Generated files, lockfiles and §7 ticks count on neither side. Choose implementation before proof-only, test-only or doc-only work in the current milestone.

## After-land review

- Named changes land through Rosen on a clean merge. Reviewers claim landed work through the review swarm and report defects with evidence.
- Rosen directs post-land verification, bisection and reverts. Velocity runs the checks and follows the results.

## Proofs

- Proof agents handle F17 proof-list features after they land. Build the feature first and keep the implementation checkable against Lean-exported fixtures. File an issue to propose a change to the proof list. Run Charon or Aeneas only when the issue requests it.

## Tests and docs

- Add a targeted test only for the claim lock, workspace deletion, persisted state surviving save and load, a proof fixture check or a concrete scout failure. Name the risk in a comment. Copy a scout's failing reproduction into `stress/` unchanged for its fix. When entry points move, preserve what the script asserts and record the adaptation in its header. Retire a script only when the hazard becomes impossible and record the risk, commit and reason in the change.
- Update the existing spec section that describes changed behaviour, one sentence per line with tables and code intact. Keep new spec rules in their own files and index them in SPEC.md. Add Markdown files only when the task calls for them.

## Sensitive paths

Keep this list aligned with `rosen.toml`. Sensitive changes take priority in review after landing.

- `rosen.toml` configures checks, merge rules and the mirror.
- `crates/velocity-store/` owns persisted state.
- `crates/velocity-git/src/lifecycle.rs` and `crates/velocity-git/src/merged_worktree.rs` guard worktree deletion.
- `crates/velocity-airc/src/claims.rs` implements the claim lock.
- `.github/workflows/`, `scripts/check.ps1` and `scripts/check-ui.mjs` run checks.

## Done means

- The feature meets its spec and runs with passing checks. The task is finished when the change lands and a green integration build covers it.

## Never

- Never modify the base directly, include secrets or `.env` files in a snapshot, or weaken a failing test to make checks pass.
