# AGENTS.md · Velocity

Velocity is a Rust + Tauri desktop IDE for running coding agents in parallel.

## Scope

- Follow the issue, §7 item or named spec section for your work. Find the applicable headings with `graph_find` (kind: section) and read them with `graph_read` (source: true). If the graph is unavailable or clips a section, read just that section's lines. Keep spec reads to the sections you need.
- SPEC.md §1 governs product rules. Agents run unrestricted with all tools and no approval gates. Providers sign in through OAuth. The frontend calls Tauri commands and subscribes to events. It never runs shell commands.
- Take the first unclaimed, unblocked item in the lowest milestone when choosing work (or call AIRC `next_work` to claim it atomically with its task brief). Use §7 for build order and §8 for open decisions that affect your piece. Skip blocked items. Treat *(proposed)* decisions as ready to build. File a `spec-question` issue via AIRC `issue_create` with the recommended answer and blocked items when a decision is missing. File contradictions and unrelated findings as issues. Edit only your claimed piece.
- Access the project Task List via AIRC's `task_list`, `task_add`, `task_update` and `task_delete` tools. Continuous dispatch threads can maintain the Task List automatically; other threads change it only at the user's request.

## Terms

- **Session card**: the short per-thread header Velocity prepends to your opening prompt with your identity, Rosen change (or git branch), held claims, overlapping changes, base tip, queue depth and any unavailable tools.
- **Scout**: an automated duty agent that sweeps landed code for bugs and attaches a failing reproduction script to each issue it files.
- **Review swarm**: automated post-land reviewers that inspect landed changes and file issues or review comments without blocking landing.
- **Receipts**: checker attestations required before landing in `before-land` Rosen projects; building agents never submit receipts in Velocity's `after-land` mode.

## Rosen workspace

- Use Rosen terms: workspace, change, snapshot, name the change, land, base, stored conflict, land history and path claim. A Rosen workspace has `.rosen`. Work in the workspace Velocity assigned and leave its cleanup to Velocity.
- Every write is snapshotted to the Rosen server automatically within 2 seconds; skip git branch, commit, push and PR steps.
- Claim every intended code path before editing with `rosen claim --exclusive <paths>` (or the `rosen` MCP `claim` tool with `exclusive: true`). The first exclusive claim wins; plain `rosen claim <paths>` notices are for registration lists and generated files, which `--exclusive` refuses. Use `rosen share <paths>` for deliberate overlap, the parking `wait` tool in the `rosen` MCP server (or `rosen wait <paths>`) for a dependency (if any path is held, end your turn and Velocity parks and resumes your thread when the paths are free) and `rosen release <paths>` to give up a claim.
- Never run `git` or `gh` directly in a Rosen workspace. Use AIRC issue tools (`issue_read`, `issue_create`, `issue_comment`, `issue_ready_to_close`, `issue_blocked_upstream`, `issue_hold`, `issue_close_not_planned`) for GitHub issues, and `workflow_run` from `velocity-run` for owner-allowed GitHub Actions workflows.
- When the thread owns a GitHub issue (shown in the session card), call AIRC `issue_comment` when you start, when a finding changes the plan and when blocked (name blocking issues in `blocked_by` and end your turn). Call `issue_blocked_upstream` for an upstream PR or release, `issue_close_not_planned` when its spec section was removed, and `issue_hold` only when physical hardware or an explicit scope hold requires a person (never for spec defaults). Call `issue_ready_to_close` with completion evidence when done and leave the issue open for Velocity.
- Run and verify working functionality before proofs, tests or docs. Use `rosen status` and `rosen diff` to inspect stored work. Name the completed change with `rosen commit -m "<summary> (#issue)"` using the assigned issue or §7 item, then end your turn. Velocity owns both the write-through watcher and landing (`rosen land`), so ignore generic `rosen --help` instructions to run `rosen land <change>` or stop the watcher manually. Never approve a review, submit receipts or land your own change.
- With `review = "after-land"`, Velocity lands a clean named change. The integration build verifies landed changes in batches. Fix a confirmed failure in the follow-up thread Velocity starts. For a stored conflict, run `rosen rebase`, combine both sides, run `rosen resolve <file>` for each conflict and name the change again.
- Address every review comment with a fix or a reason. Check fixes and name the change again.

## Shared files

- Velocity ticks the owning §7 item automatically when a Rosen change lands, so agents do not edit SPEC.md §7 in every change. Register commands, events, modules, members and dependencies in the appropriate group and place new named groups alphabetically. Use the existing implementation if the base lands the same fix first.
- When Rosen reports `regenerate`, queue `cargo metadata --format-version 1` via `queue_run` for `Cargo.lock`, run `pnpm -C ui install --lockfile-only` for `ui/pnpm-lock.yaml`, and queue `pnpm -C ui bindings` via `queue_run` for `ui/src/lib/bindings.ts`. Regenerate from the merged inputs and name the change again. Keep generated artifacts and registration lists in their respective `rosen.toml` lists.

## Commands

- Run `preflight` from `velocity-run` before naming the change, then end your turn and continue from the pass/fail report Velocity sends. It inspects the change's diff and runs only the checks that apply (`rustfmt --check` on changed Rust files, UI type-check and lint when `ui/` changed, queued `cargo clippy -p <crate> --all-targets -- -D warnings` for every touched Rust crate, active overlaps and generated-file freshness). Velocity records a pass for the checked snapshot (and a Rosen receipt on `after-land` projects) and refuses to name or land a change whose latest snapshot has no passing preflight.
- Always queue checks and builds through `queue_run` from `velocity-run` (including `cargo check -p <crate>`, Lean checks, `cargo metadata --format-version 1`, `pnpm -C ui bindings`, and any generator registered under `[generated.commands]` in the base's `rosen.toml`), passing the program and its arguments separately (plus `env` when a registered generator sets environment variables), then end your turn and continue from Velocity's result. Registered `[generated.commands]` generators run in the light run queue and may change only paths covered by their registered pattern(s); any other change is reverted and refused. Inspect or cancel queued runs with `run_status` and `run_cancel`. Fall back to `rosen run -- <command>` only when `velocity-run` is listed under `Unavailable tools` in the session card.
- The `cargo` shim on `PATH` allows only `cargo fmt`, `cargo check -p <crate>` (queue via `queue_run`), managed `cargo metadata --format-version 1`, and queued `[generated.commands]` generators, and refuses other Cargo or Rust compiler invocations (`build`, `test`, `clippy`, `run`, `rustc`) because the integration build covers full compilation, clippy and tests.
- Format only changed Rust files (`rustfmt --edition 2024 <files>`) rather than `cargo fmt --all`, which can rewrite unclaimed files. For UI formatting use `pnpm -C ui format`. The integration build runs full Rust compilation, clippy, tests and bindings verification; the release gate runs stress.
- Use `velocity-browser` for web pages: read pages with `snapshot`, interact using element refs, request `screenshot` only for visual checks, never enter passwords or tokens, and ask the owner to use Take over in the Browser tab for sign-in.

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
