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
- All local checks: `pwsh -NoProfile -File scripts/check.ps1`. It runs every check PR duty approves on: format, locked build, each crate built on its own, generated bindings, clippy, UI check, lint and build, and `lake build` with the axiom audit when `proofs/` changes. Run it on the merge result before you mark a PR ready.
- One-time machine setup: `pwsh -NoProfile -File scripts/Setup-DevMachine.ps1`. It registers the merge driver for generated files and turns on the shared sccache compiler cache.
- Heavy builds go through the build queue: `pwsh -NoProfile -File scripts/build-queue.ps1 cargo build --workspace`. Use it for `cargo build`, `cargo test`, `cargo clippy` and `cargo run`. It runs one build at a time on this machine, so each gets every core and finishes several times faster than five builds side by side. `scripts/check.ps1` already uses it. While you wait, it prints who holds the queue; never stop that process to jump the queue. Never run `cargo tauri dev` through it: the dev server runs until stopped and would hold the queue.
- Proofs: `lake build` in `proofs/`. Run it before every push that changes `proofs/`.
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
- `crates/velocity-interview/`: the spec interview (F3): runs the spec agent in the main checkout, parses its questions, records decisions and draft tasks, and locks the spec.
- `ui/`: the frontend: Svelte 5, TypeScript and Vite, built with pnpm.
- `proofs/`: the Lean 4 Lake project.
- `stress/`: tests and stress scripts. Scouts and PR duty agents own it.
- `design/`: the reference UI. Read-only.
- `scripts/`: development tooling: the local checks, machine setup and the PR duty app token.
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
- Add every new field on a saved record in `velocity-store` with `#[serde(default)]`, so an existing `state.json` still loads.
- Build a struct that another module defines through a function that module provides (`new`, `Default`, a builder), never a full struct literal. If none exists, add one in that module. A field another PR adds then can't break your code after a clean merge.

## UI
- Svelte 5 with runes (`$state`, `$derived`, `$effect`, `$props`). Never use Svelte 4 stores or `export let`.
- Plain Vite. No SvelteKit: Velocity is one window with no routes and no server.
- TypeScript in strict mode. Never use `any`, `@ts-ignore` or `@ts-expect-error`.
- Every Tauri command and event goes through the IPC layer, `ui/src/lib/ipc.ts` and `ui/src/lib/ipc/`. Components never call `invoke` or `listen` directly.
- Put a new feature's IPC wrappers and type re-exports in their own file, `ui/src/lib/ipc/<feature>.ts`, and add one line to `ipc.ts`: `export * from './ipc/<feature>';`. Never append to the shared lists in `ipc.ts`.
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
- Take the first unclaimed item that nothing blocks, from the lowest milestone that has one. An item is blocked when it depends on an open question in §8, or on an unmerged PR. Skip blocked items; never wait for them.
- Never build anything that depends on an open question in SPEC.md §8. File an issue that names the question, labelled `spec-question`, with your recommended answer and the items it blocks, then pick other work.
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

## Shared files
Most merge conflicts come from a few files every feature touches. Keep your edits to them conflict-free:
- **Generated files:** `ui/src/lib/bindings.ts`, `Cargo.lock` and `ui/pnpm-lock.yaml`. Never resolve a conflict in them by hand. After `scripts/Setup-DevMachine.ps1`, `git merge origin/master` keeps master's copy without stopping. Then regenerate and commit: `pnpm -C ui bindings`, `cargo build --workspace` for `Cargo.lock`, `pnpm -C ui install` for `pnpm-lock.yaml`. `scripts/check.ps1` fails if you forget.
- **Registration lists:** `collect_commands!`, `collect_events!`, `.typ` and `.constant` in `src-tauri/src/lib.rs`, the `mod` lines in each crate's `lib.rs`, and the dependency and member lists in `Cargo.toml`. Add your lines inside your module's group. A new module's group goes in alphabetical order, never at the end: two PRs that both append at the end always conflict.
- Edits to generated files and registration lists never need a "shares" note (Coordination).
- **`.gitattributes`:** `/stress/*` already preserves every stress script's bytes. Never add a line per script.
- **SPEC.md §7:** never tick a checkbox in your PR. PR duty ticks it when it lands the PR.
- **Overlapping fixes, first come, first served:** when two open PRs fix the same root problem or add the same helper, the one that lands first wins. The later PR drops its own version and switches to `master`'s in the commit that merges `master` in. Never land two implementations of one thing, and never hold a PR back so another can land its version first.
- **Silent conflicts:** after merging `master`, a build error in code neither side edited (a struct gained a field, a function was renamed) is a merge conflict. Fix it in the merge commit.

## Pull requests
- One piece, one branch, one PR, against `master`.
- PR description: what changed, why, the theorems you proved, and the piece it completes (§7 item, `Fixes #n`, or SPEC section).
- Name your §7 item in the PR description. Never tick its checkbox yourself: PR duty ticks it when it lands the PR.
- Opening the PR does not finish the task. Monitor the PR until it is merged:
  - If the local checks or CI fail on your change, fix it and push.
  - Address every comment PR duty agents leave: fix it and push, or reply with the reason you didn't. Resolve the thread.
  - After pushing fixes, re-request review.
  - PR duty agents may update your branch with `master`, and push small fixes to it, at any time. Pull before you push again (`git pull --no-rebase`).
  - Merge `master` into your branch only before you mark the PR ready, or when PR duty sends a conflict back to you. While the PR has the `reviewing` label, PR duty owns branch updates: a merge you push at the same time throws away the check it is running. Never force-push a branch PR duty may have updated.
- If the PR is closed without merging, report why in your thread and keep your worktree.

## If you are on PR duty
- Hold one open, ready PR at a time. To take one: pick a ready PR without the `reviewing` label, add the label (`gh pr edit <n> --add-label reviewing`), comment "Reviewing · <your worktree slug>", then read the PR's comments again. If another agent's "Reviewing" comment is earlier than yours, leave the label and pick another PR.
- Never review, update or merge a PR another agent holds. Remove the `reviewing` label when you merge, send the PR back to its author, or stop.
- Never sit idle while a ready PR waits. Each time you merge, send a PR back, or would otherwise wait, list open PRs again and take the oldest ready PR without the `reviewing` label. Wait only when every ready PR is held by another agent or waiting on its author.
- Review, approve, request changes and merge as the PR duty GitHub app, not the shared login, so approvals are real GitHub reviews (#42). Set the token in the same command as `gh`: `$env:GH_TOKEN = pwsh -NoProfile -File scripts/Get-PrDutyToken.ps1; gh pr review <n> --approve`. Approve with `gh pr review --approve` and block with `gh pr review --request-changes`, never with a plain comment. If the script says its settings are missing, the app isn't set up on this machine: record approval as a comment starting `Approved at <head SHA>`.
- Review the intent, not each line: check the PR against its claim, its SPEC.md section and this file.
- Request changes on any PR that adds Lean for a feature not on the proof list, or a theorem about a single behaviour or case.
- Review every theorem statement: does it say what the SPEC section says, do its hypotheses exclude real inputs, is the cost model honest, and is the class of alternatives the one that matters? Lean already checked the proofs.
- When you doubt a claim no proof covers, write a stress script in `stress/`, run it, and attach the result to your comment. Point its `CARGO_TARGET_DIR` at your review worktree's `target/` so it reuses the crates already compiled there.
- Request changes on a PR whose proofs, tests and docs together outweigh its implementation, or that adds tests outside the critical contracts in Tests and docs.
- Approve only when `scripts/check.ps1` passes on the PR merged onto the current `master`, in your review worktree. It runs format, the locked build, each crate built on its own, bindings, clippy, `pnpm -C ui check`, `pnpm -C ui lint`, `pnpm -C ui build`, and, when `proofs/` changes, `lake build` with the axiom check above.
- GitHub CI never holds up a PR. A pending, red or stale run doesn't block a merge when the local checks pass on the merge result.
- After another PR merges, run the checks again on the new merge result before you merge the next one.
- Update PR branches with `master`. You are allowed and encouraged to, so PRs never wait on their author to rebase:
  - Whenever a PR you review or are about to merge is behind `master`, run `gh pr update-branch <n>`, or merge `master` into it in your review worktree and push.
  - Resolve a conflict yourself when the resolution keeps both changes as written, or when both sides implement the same thing: keep `master`'s version and adapt the PR to it (first come, first served, see Shared files). Send it back to the author only when it means choosing between two different behaviours, or adapting takes more than a few lines.
  - Never force-push, and never rewrite the author's commits.
  - Run the checks on the updated branch, not the old PR head.
- Land a PR in this order: update the branch with `master`, tick its §7 checkbox in SPEC.md in a commit on the branch, run `scripts/check.ps1`, approve, then merge with `gh pr merge <n> --squash --delete-branch --match-head-commit <the SHA you approved>`. A commit pushed after your approval then stops the merge.
- Never merge without an approving review.
- If a PR merges while you are still reviewing it, file your findings as issues. Never post them on the merged PR.

### Review severity
Long review cycles cost more than the defects they catch. Block only what matters:
- Request changes only for: data loss or corruption; a crash or hang; a violation of SPEC.md §1 or the Product rules; a security hole; a failing local check; the main path of the claimed piece not doing what its SPEC.md section says; or a Lean or tests rule above.
- Everything else does not block: edge cases on rare inputs, display miscounts, naming, style, missing polish. File each as an issue with its evidence, link it in your review, and approve.
- Fix a blocking finding yourself when the fix is a few lines with one obvious correct form, such as a missing derive, a wrong guard or a dropped field. Push it as your own commit on the branch, with its stress script in `stress/`, say what you changed in your review, and carry on landing the PR. Send the PR back only when the fix needs a design choice or is larger.
- Put every blocking finding in your first review. A later round reviews only the commits pushed since your last review, and blocks only on a finding in those commits, or on new data loss or a crash.
- When you are unsure whether a finding blocks, it does not block. File the issue.

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
