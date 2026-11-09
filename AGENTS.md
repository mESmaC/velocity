# AGENTS.md · velocity

Read this file in full before you start any task. Every rule here is mandatory.

You are building Velocity: a Rust + Tauri desktop IDE that runs many coding agents in parallel.
- `SPEC.md` defines what to build. §1 holds the product rules; §7 is the build order; §8 lists open questions.
- `design/main-window.html` is the reference UI. Open it in a browser. Every feature in SPEC.md is mocked there with live simulated data, and its behaviour is part of the spec.

## Implementation first
Your job is working functionality. Proofs, tests and docs support it; they never replace it.
- Build the feature first, end to end, then run it (`cargo tauri dev`, or the crate's own entry point) and see it work.
- Only then add the proof, test or doc the piece needs (see Proofs, and Tests and docs).
- Implementation is the larger part of every PR. Proofs, tests and docs together stay smaller than the implementation code. Lockfiles, generated files and §7 checkbox ticks count on neither side.
- Never claim a piece that is only tests, proofs or docs while an implementation piece in the current milestone is unclaimed.

## Commands
- Build: `cargo build --workspace`
- Run the app: `cargo tauri dev`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`. Zero warnings.
- Format: `cargo fmt --all`. Run it before every commit.
- Proofs: `lake build` in `proofs/`. Run it before every push.
- Proof toolchain setup (Windows + Ubuntu WSL): `powershell -File proofs/scripts/Setup-Toolchain.ps1`.
- Charon and Aeneas (only when an issue asks for a translation): `powershell -File proofs/scripts/Export-Rust.ps1 -CratePath crates/<crate> -Feature <Feature> -StartFrom crate::<module>`. This runs pinned `charon cargo --preset=aeneas` then `aeneas -backend lean`; complete the confidence proof and run `lake build` afterwards. See `proofs/README.md`.
- Stress: `cargo run -p stress -- --all`. Scouts and PR duty agents only.
- UI type check: `pnpm -C ui check` (svelte-check). Zero errors and zero warnings.
- UI lint: `pnpm -C ui lint`. Zero warnings.
- UI format: `pnpm -C ui format`. Run it before every commit.
- `cargo tauri dev` starts the UI dev server itself. Never start it separately.
- The PR that sets up `ui/` creates these three scripts in `ui/package.json`.

## Layout
- `src-tauri/`: the Tauri app. Tauri commands and events only: it calls into the crates below and holds no logic of its own.
- `crates/velocity-core/`: state, projects, threads, tasks, settings. No Tauri, no I/O.
- `crates/velocity-store/`: JSON persistence store (`state.json`) for projects, threads, tasks and settings.
- `crates/velocity-git/`: git and worktrees.
- `crates/velocity-acp/`: the ACP client and agent processes.
- `crates/velocity-airc/`: claims, events, ledgers and the AIRC MCP server.
- `crates/velocity-github/`: GitHub through `gh`.
- `crates/velocity-threads/`: starting build threads from the composer (F4): the thread's name and worktree plan, its live agent session, and the transcript the thread view shows.
- `crates/velocity-new-project/`: Create in the New project dialog (F2): resolves the request and runs the planned git, `gh` and store steps.
- `ui/`: the frontend: Svelte 5, TypeScript and Vite, built with pnpm.
- `proofs/`: the Lean 4 Lake project.
- `stress/`: tests and stress scripts. Scouts and PR duty agents own it.
- `design/`: the reference UI. Read-only.
- If your work needs a crate that isn't listed here, add it to this list in the same PR.

## Product rules
SPEC.md §1 overrides everything else. Never build any of these:
- An approval prompt, a plan-approval gate or a "needs you" state. Agents always run unrestricted (Rule 1).
- A field, file or setting that holds an API key. Every provider signs in with OAuth (F15).
- A way for the frontend to run a shell command. The frontend calls Tauri commands and subscribes to events (§3).
- A tool restriction on any agent: no allow-lists, deny-lists, read-only modes or sandboxes (§3.2 Tool access).

## Code
- Rust edition 2024, stable toolchain, pinned in `rust-toolchain.toml`.
- No `unsafe`. If you need it, file an issue that explains why, and stop that piece of work.
- Errors: `thiserror` in the crates, `anyhow` in `src-tauri`. Never call `unwrap()` or `expect()` outside `stress/`.
- Keep functions under 60 lines. Split anything longer.
- Name functions, types and variables for what they do. No abbreviations.
- Use the IPC names in SPEC.md §3.1. If you need a new command or event, add it to §3.1 in the same PR.
- Put logic you prove in pure, synchronous functions in the crates. Keep I/O, async and Tauri at the edges, so the logic can be checked against fixture tables (see Proofs).
- Handle Windows paths everywhere: drive letters, spaces, backslashes and paths over 260 characters. Windows 11 is the primary platform.
- Comment code whose purpose isn't clear from reading it. Explain why, not what.

## UI
- Svelte 5 with runes (`$state`, `$derived`, `$effect`, `$props`). Never use Svelte 4 stores or `export let`.
- Plain Vite. No SvelteKit: Velocity is one window with no routes and no server.
- TypeScript in strict mode. Never use `any`, `@ts-ignore` or `@ts-expect-error`.
- Every Tauri command and event goes through `ui/src/lib/ipc.ts`. Components never call `invoke` or `listen` directly.
- The TypeScript types for commands and events are generated from Rust with `tauri-specta`. Never write them by hand.
- Keep logic out of the UI. State changes happen in Rust; components render state and send commands.
- Copy `:root` from `design/main-window.html` exactly into `ui/src/lib/theme/tokens.css`.
- Missing values go in `ui/src/lib/theme/reference.css`, copied from the reference's winning CSS declarations. The user authorized this supplement in #12. Keep `tokens.css` and `design/` unchanged when adding supplemental values.
- Take every colour, radius, font and size from these token files. Components use `var(--token)` only. Never hard-code a visual value or derive a missing value from unrelated tokens.
- Black is `#000`. Panes are smoked glass on black, as in the reference. No Mica or vibrancy.
- No italics anywhere.
- A screen is done when it matches the reference UI's layout, states and keyboard behaviour (SPEC.md §5).

## Proofs
Lean proves a few algorithms where a mistake loses work or breaks coordination. It never blocks implementation, and most pieces have no Lean.
- Write Lean only for features on the proof list in SPEC.md F17: claim lock, targeted rebases and landing, worktree deletion, goal swarm work choice, and stall detection and claim release. Every other piece ships with no Lean.
- If you think a feature belongs on the proof list, file an issue. Never write a proof for a feature that isn't on it.
- For a proof-list feature, write the code first. Then, in the same PR, write `proofs/<Feature>/Design.lean`. It states exactly three theorems:
  - `correct`: the model meets the SPEC.md section it implements. Name that section in the file.
  - `cost_bound`: its cost stays within a bound, under a cost model the file defines.
  - `optimal`: no alternative in a class the theorem states costs less.
- Lemmas are allowed only as steps toward those three theorems.
- Never write a theorem about one behaviour, case, key, value or example. That is a test.
- State the cost model and the class of alternatives in the theorem itself. A proof against an unstated alternative does not count.
- If a design proof passes 300 lines, stop and file an issue. The model is too detailed.
- Tie the proof to the Rust with fixtures, not a translation. Export a compact fixture table from the Lean model to `proofs/fixtures/<feature>.tsv`, and add one Rust test that checks the implementation against it.
- Never run Charon or Aeneas unless an issue asks for it.
- A bug fix in a proof-list feature adds the failing input to its fixture table. A bug fix anywhere else has no Lean.
- Never use `sorry`, `admit`, `axiom` or `native_decide`. `#print axioms` lists only `propext`, `Classical.choice` and `Quot.sound`.
- Never change a theorem statement after review to make its proof go through.
- `proofs/` is the pinned Lake project. `proofs/toolchain-pins.json` records the compatible Lean, Mathlib, Charon and Aeneas versions. Keep its pins, `lean-toolchain`, `lakefile.lean` and `lake-manifest.json` consistent; follow `proofs/README.md` when updating them.

## Tests and docs
Both are low priority. Add them only where they pay for themselves, and only after the feature works.
- Default: add no tests.
- Add a targeted test only for a critical contract: the claim lock, worktree deletion, saved state surviving a save and load, the fixture check for a proof-list feature, or a concrete failure a scout found. Put the risk it covers in a comment above it.
- Never write a test that restates the implementation, and never add a test suite for a whole module.
- Scouts and PR duty agents write the adversarial tests and stress scripts.
- A bug fix copies the scout's failing script into `stress/` unchanged. Never edit it.
- Docs: update SPEC.md when behaviour changes, and comment code whose purpose isn't clear. Never add a new markdown file unless the piece asks for one.

## Choosing work
- Work comes from three places: unchecked items in SPEC.md §7, open GitHub issues, and SPEC.md sections nothing implements yet.
- Take the lowest milestone that still has unclaimed items. Within it, take the first unclaimed item.
- Never build anything that depends on an open question in SPEC.md §8. File an issue that names the question, then pick other work.
- Items marked *(proposed)* are decided enough to build. Build them as written.
- If SPEC.md is wrong or contradicts itself, file an issue. Never change a decision in SPEC.md yourself.
- If your change alters behaviour SPEC.md describes, update SPEC.md in the same PR.
- Work only on the piece you claimed. If you find an unrelated problem, file an issue for it. Do not fix it in your PR.

## Coordination (claims)
Velocity's AIRC (F14) replaces this section once M8 merges. Until then, claims are draft PRs:
- Before you edit anything, claim your piece:
  1. List open PRs: `gh pr list --state open --json number,title,body,createdAt,headRefName`.
  2. Pick a piece no open PR claims.
  3. Create your branch and worktree (see Worktrees), push an empty commit, and open a draft PR against `master` titled `[claim] <the §7 item, issue or SPEC section>`.
  4. In the PR body, list the piece and every file you expect to touch.
- First claim wins. List open PRs again. If an earlier open PR claims the same piece, close yours, delete your branch and worktree, and pick again.
- Before you edit a file another open PR lists, write "shares `<file>` with #n" in your PR body and comment the same on #n.
- If your piece needs another PR merged first, write "Waits for #n" in your PR body. Rebase onto `master` after #n merges.
- If you touch a file your PR body doesn't list, add it to the list before you push.
- When the work is done, retitle the PR without `[claim]` and mark it ready for review.

## Worktrees
- Every agent works in its own git worktree under `X:\velocity-wt\`. Create it from the latest `master`: `git fetch origin` then `git worktree add X:\velocity-wt\<slug> -b vel/<slug> origin/master`.
- Branch name: `vel/<slug>`, where the slug is a short name for the piece.
- After your PR merges, remove your worktree (`git worktree remove X:\velocity-wt\<slug>`) and delete your local branch.
- Never touch another agent's worktree or branch. Never run `git worktree prune`. The one exception: PR duty agents update PR branches with `master` (see If you are on PR duty).

## Pull requests
- One piece, one branch, one PR, against `master`.
- PR description: what changed, why, the theorems you proved, and the piece it completes (§7 item, `Fixes #n`, or SPEC section).
- Tick the §7 checkbox for your item in the same PR.
- Opening the PR does not finish the task. Monitor the PR until it is merged:
  - If the local checks or CI fail on your change, fix it and push.
  - Address every comment PR duty agents leave: fix it and push, or reply with the reason you didn't. Resolve the thread.
  - After pushing fixes, re-request review.
  - PR duty agents may update your branch with `master` at any time. Pull before you push again (`git pull --no-rebase`).
  - If `master` moves and your PR conflicts, merge `master` into your branch, resolve the conflict and push. Never force-push a branch PR duty may have updated.
- If the PR is closed without merging, report why in your thread and keep your worktree.

## If you are on PR duty
- Claim one open, ready PR at a time by commenting "Reviewing". Never review a PR another agent claimed.
- Review the intent, not each line: check the PR against its claim, its SPEC.md section and this file.
- Request changes on any PR that adds Lean for a feature not on the proof list, or a theorem about a single behaviour or case.
- Review every theorem statement: does it say what the SPEC section says, do its hypotheses exclude real inputs, is the cost model honest, and is the class of alternatives the one that matters? Lean already checked the proofs.
- When you doubt a claim no proof covers, write a stress script in `stress/`, run it, and attach the result to your comment.
- Request changes on a PR whose proofs, tests and docs together outweigh its implementation, or that adds tests outside the critical contracts in Tests and docs.
- Approve only when the local checks pass on the PR merged onto the current `master`, in your review worktree: `cargo fmt --all --check`, `cargo build --workspace --locked`, clippy, `pnpm -C ui check`, `pnpm -C ui lint`, `pnpm -C ui build`, and, when `proofs/` changes, `lake build` with the axiom check above.
- GitHub CI never holds up a PR. A pending, red or stale run doesn't block a merge when the local checks pass on the merge result.
- After another PR merges, run the checks again on the new merge result before you merge the next one.
- Update PR branches with `master`. You are allowed and encouraged to, so PRs never wait on their author to rebase:
  - Whenever a PR you review or are about to merge is behind `master`, run `gh pr update-branch <n>`, or merge `master` into it in your review worktree and push.
  - Resolve a conflict yourself when the resolution keeps both changes as written. When it means choosing between two behaviours, send it back to the author.
  - Never force-push, and never rewrite the author's commits.
  - Run the checks on the updated branch, not the old PR head.
- Never merge without an approving review. Merge with squash and delete the branch.

## If you are a scout
- Assume every part of Velocity is broken and will fail. Prove it.
- Write tests and stress scripts in `stress/`: load, concurrency, malformed input, resource exhaustion, crash and restart, and Windows paths.
- Look for proof gaps too: features with no proof, statements weaker than SPEC.md, hypotheses that exclude real inputs.
- File every issue with its evidence attached: the failing test or stress script, and its output.
- Never file a duplicate. Search open and closed issues first.

## Done means
- The piece is built exactly as SPEC.md describes it and behaves like the reference UI. You ran it and saw it work.
- The proofs check, clippy, `pnpm -C ui check` and `pnpm -C ui lint` report zero warnings on the merge result.
- The PR is merged and your worktree is removed.

## Never
- Never push to `master` directly.
- Never merge your own PR. PR duty agents merge.
- Never force-push to another agent's branch.
- Never edit `design/`. Changes to the reference UI come from the user.
- Never skip git hooks (`--no-verify`).
- Never commit secrets, tokens or `.env` files.
- Never skip, delete or weaken a scout's test or stress script to make anything pass.
