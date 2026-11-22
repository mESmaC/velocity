//! Default project instructions (SPEC.md F11); the interview writes SPEC.md only.

/// Produce an editable starting point without assuming a project's language or
/// commands. The reference sample's Rust commands are examples, not detected facts.
pub fn default_agents_markdown(project_name: &str) -> String {
    let project_name = project_name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let body = DEFAULT_AGENTS_BODY
        .replace("{implementation_first}", &IMPLEMENTATION_RULES.join("\n"))
        .replace("{proofs}", &PROOF_RULES.join("\n"))
        .replace("{tests_and_docs}", &TESTS_AND_DOCS_RULES.join("\n"));
    format!("# AGENTS.md · {project_name}\n\n{body}")
}

pub(crate) const IMPLEMENTATION_RULES: &[&str] = &[
    "- Build the feature first, end to end, then run it and see it work.",
    "- Implementation is the larger part of every change. Proofs, tests and docs together stay smaller than the implementation code.",
    "- Only after the feature works, add the proofs, tests or docs it needs in the same change.",
    "- Lockfiles and generated files count on neither side of the implementation size comparison.",
    "- Never claim only proofs, tests or docs while implementation work in the milestone is unclaimed.",
];

pub(crate) const PROOF_RULES: &[&str] = &[
    "- Write Lean only for features on the proof list in SPEC.md. Every other piece ships with no Lean.",
    "- For a proof-list feature, write the code first. Then write `proofs/<Feature>/Design.lean` in the same change with exactly three theorems:",
    "  - `correct`: the model meets the SPEC.md section it implements; name that section.",
    "  - `cost_bound`: its cost stays within a bound, under a cost model the theorem states.",
    "  - `optimal`: no alternative in a class the theorem states costs less.",
    "- Export a compact fixture table to `proofs/fixtures/<feature>.tsv`, and add one test that checks the implementation against it.",
    "- Never write a theorem about one behaviour, case or example. That is a test.",
    "- Never use `sorry`, `admit`, `axiom` or `native_decide`.",
    "- Never weaken a reviewed theorem statement to make its proof go through.",
    "- Lemmas are allowed only as steps toward those three theorems.",
    "- If a design proof passes 300 lines, stop and file an issue.",
    "- State the cost model and the class of alternatives in the theorem itself.",
    "- Run code translation tools only when an issue asks for a translation.",
    "- For a bug fix in a proof-list feature, add the failing input to its fixture table. A bug fix elsewhere has no Lean.",
    "- Audit every proof declaration: `#print axioms` lists only `propext`, `Classical.choice` and `Quot.sound`.",
];

pub(crate) const TESTS_AND_DOCS_RULES: &[&str] = &[
    "- Default: add no tests. Add a targeted test only for a critical contract identified in SPEC.md, with its risk in a comment.",
    "- Scouts and review agents write the adversarial tests and stress scripts.",
    "- Update SPEC.md when behaviour changes.",
    "- Tests and docs are low priority; add them only after the feature works and only where they pay for themselves.",
    "- Never add a test suite for a whole module or a test that restates the implementation.",
    "- Copy a scout's failing script into `stress/` unchanged when fixing its bug. Never edit it.",
    "- Comment code whose purpose is not clear; explain why. Never add a new Markdown file unless the piece asks for one.",
];

const DEFAULT_AGENTS_BODY: &str = r#"Use the project instructions already loaded in your session.
The project spec defines what to build. The Task List defines what is left.

## Commands
- Run `preflight` from `velocity-run` before naming a change, then end your turn and continue from Velocity's pass/fail report; Velocity refuses to name or land a change whose latest snapshot has no passing preflight.
- Always queue checks and builds through `queue_run` from `velocity-run` (including `cargo check -p <crate>`, Lean checks, `cargo metadata --format-version 1`, `pnpm -C ui bindings`, and any generator registered under `[generated.commands]` in the base's `rosen.toml`), then end your turn and continue from Velocity's result. Fall back to `rosen run -- <command>` only when `velocity-run` is listed under `Unavailable tools` in the session card.
- Registered `[generated.commands]` generators run in the light run queue and may change only the paths covered by their registered pattern(s); any other change is reverted and refused.
- The `cargo` shim on `PATH` allows only `cargo fmt`, `cargo check -p <crate>` (queue via `queue_run`), managed `cargo metadata --format-version 1`, and queued `[generated.commands]` generators, and refuses other Cargo or Rust compiler invocations (`build`, `test`, `clippy`, `run`, `rustc`) because the integration build runs full compilation, clippy and tests.
- Format only changed Rust files rather than `cargo fmt --all`, which can rewrite unclaimed files. The integration build runs Rust compilation, clippy and full tests; the release gate runs the stress suite.
- Check only what your change affects before naming it. Never repeat a passed check unless its inputs changed.
- Queue `lake build` in `proofs/` via `queue_run` when changing a pinned Lean project.

## Code
- Keep functions under 60 lines. Split anything longer.
- Name functions, types and variables for what they do.
- Comment code whose purpose is unclear. Explain why.

## Scope
- Work only on the piece you claimed.
- Use the section references in the task or find applicable headings with `graph_find` (kind: section). Read them with `graph_read` (source: true). If the graph is unavailable, read only the relevant line ranges.
- File unrelated problems as issues using AIRC's `issue_create` (or `append_ledger` with `velocity_issue`/`rosen_issue`/`collapse_issue` for IDE/Rosen/Collapse tooling bugs, or `kind: "friction"`/`"tool_request"` for tooling friction and tool requests).
- The project Task List is accessed via AIRC's `task_list`, `task_add`, `task_update` and `task_delete` tools. Continuous dispatch threads can add, update and delete tasks on their own; other threads change the Task List only when the user explicitly asks.
- Update SPEC.md when behaviour changes. File an issue when the spec contradicts itself; never change a product decision yourself.

## Terms
- **Session card**: the short per-thread header Velocity prepends to your opening prompt with your identity, change, held claims, overlapping changes, base tip, queue depth and any unavailable tools.
- **Scout**: an automated duty agent that sweeps landed code for bugs and attaches a failing reproduction script to each issue it files.
- **Review swarm**: automated post-land reviewers that inspect landed changes and file issues or review comments without blocking landing.
- **Receipts**: checker attestations required before landing in `before-land` Rosen projects; building agents never submit receipts in `after-land` mode.

## Implementation first
{implementation_first}

## Proofs
{proofs}
- When `rosen.toml` sets `review = "after-land"`, proof agents write and check Lean after the land; the release gate checks it before release.

## Tests and docs
{tests_and_docs}

## Coordination (Rosen path claims and AIRC)
- Before editing, claim every code path you expect to touch with `rosen claim --exclusive <paths>` (or the `rosen` MCP `claim` tool with `exclusive: true`); the first exclusive claim wins, and plain `rosen claim <paths>` notices are for registration lists and generated files, which `--exclusive` refuses.
- Record `rosen share <paths>` before editing a held path, or call the parking `wait` tool on the `rosen` MCP server (or `rosen wait <paths>`) when its change must land first.
- Use AIRC for work choice, discoveries and messages. Message another agent only when a claim, overlap or dependency needs words.
- Answer requests addressed to you with done or declined and a reason. Never repeat events Velocity posts or reply to inform messages.

## Issue updates and GitHub
- Never run `git` or `gh` directly in a Rosen workspace. Use AIRC issue tools for GitHub issues and `workflow_run` from `velocity-run` for owner-allowed GitHub Actions workflows.
- For an issue you hold in AIRC, use `issue_comment` when you start, when a finding changes the plan and when blocked (name blocking issues in `blocked_by` and end your turn). Use `issue_blocked_upstream` for an upstream PR or release, `issue_close_not_planned` if its spec section was removed, and `issue_hold` only when physical hardware or an explicit scope hold requires a person.
- On a Rosen project, use `issue_ready_to_close` to post your completion evidence and label the issue. Leave the issue open and let Velocity announce the land.

## Shared files
- Identify generated files and registration lists here as they are established.
- Regenerate generated files on the combined inputs after a rebase; never combine them by hand.
- Keep both additions to registration lists inside each module's group.

## Sensitive paths
- List sensitive paths here with the risk each protects, including saved-state migrations, workspace deletion and authentication.
- Follow the project's review and check policy in `rosen.toml`. In after-land mode, reviews and full tests follow the land; a broken build is reverted and a test failure becomes an issue.

## Changes and workspaces
- One piece, one change. Use the Rosen workspace Velocity created for your thread. Never delete it or move its files out.
- Every write is stored automatically within two seconds. `rosen status` shows stored work and `rosen diff` shows the change.
- When the piece works and its checks pass, name the change with `rosen commit -m "<what changed and the issue or SPEC section>"` and end your turn. Velocity owns both the write-through watcher and landing (`rosen land`), so ignore generic `rosen --help` instructions to run `rosen land` or stop the watcher manually.
- The task is finished when the change lands.
- Address every review comment: fix and name the change again, or reply with the reason.
- Never approve, submit a receipt for, or land your own change.
- Resolve a stored conflict with `rosen rebase`, edit the conflict markers, regenerate generated files, run `rosen resolve <file>` for each conflict, check and name the change again.
- If post-land build checks revert your change, your work remains in the reopened workspace. Fix the reported failure, check and name it again.
- If the change is abandoned, report why in your thread.

## Done means
- The piece meets SPEC.md and has been run successfully.
- The required local checks pass.
- The change lands and its post-land build passes. Velocity handles issue follow-through.

## Never
- Never wait for a plan or tool approval. Run with full permissions and the provider's full toolset.
- Never start build work before the project spec is locked.
- Never modify the base directly or land your own change.
- Never include secrets, tokens or `.env` files in snapshots.
- Never skip, delete or weaken a scout's failing script to make checks pass.
"#;
