//! Default project instructions (SPEC.md F11); the interview writes SPEC.md only.

/// Produce an editable starting point without assuming a project's language or
/// commands. The reference sample's Rust commands are examples, not detected facts.
pub fn default_agents_markdown(project_name: &str) -> String {
    let project_name = project_name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!("# AGENTS.md · {project_name}\n\n{DEFAULT_AGENTS_BODY}")
}

const DEFAULT_AGENTS_BODY: &str = r#"Read this file in full before you start any task. Every rule here is mandatory.
SPEC.md defines what to build. The project Task List defines what is left.

## Commands
- Read the project's build, run and lint commands from its manifests and SPEC.md before running them. Record those commands here when they are established.
- For a project with a pinned Lean project, run `lake build` in `proofs/` before pushing proof changes.
- Run the project's local checks on the merge result before marking a PR ready. Lint reports zero warnings.

## Code
- Keep functions under 60 lines. Split anything longer.
- Name functions, types and variables for what they do.
- Comment code whose purpose is not clear from reading it. Explain why, not what.

## Scope
- Work only on the piece you claimed.
- If you find an unrelated problem, file an issue for it. Do not fix it in your PR.
- Add to the Task List only when the user asks. Otherwise, file an issue.
- Update SPEC.md when behaviour changes. Never change a product decision yourself; file an issue when the spec contradicts itself.

## Implementation first
- Build the feature first, end to end, then run it and see it work.
- Implementation is the larger part of every PR. Proofs, tests and docs together stay smaller than the implementation code.

## Proofs
- Write Lean only for features on the proof list in SPEC.md. Every other piece ships with no Lean.
- For a proof-list feature, write the code first. Then write `proofs/<Feature>/Design.lean` in the same PR with exactly three theorems:
  - `correct`: the model meets the SPEC.md section it implements; name that section.
  - `cost_bound`: its cost stays within a bound, under a cost model the theorem states.
  - `optimal`: no alternative in a class the theorem states costs less.
- Export a compact fixture table to `proofs/fixtures/<feature>.tsv`, and add one test that checks the implementation against it.
- Never write a theorem about one behaviour, case or example. That is a test.
- Never use `sorry`, `admit`, `axiom` or `native_decide`.
- Never weaken a reviewed theorem statement to make its proof go through.

## Tests and docs
- Default: add no tests. Add a targeted test only for a critical contract identified in SPEC.md, with its risk in a comment.
- Scouts and PR duty agents write the adversarial tests and stress scripts.
- Update SPEC.md when behaviour changes.

## Coordination (AIRC)
- Before editing, claim your task, branch and every file you expect to touch in AIRC.
- If your claim is refused, pick different unclaimed work.
- Before editing a file another agent claimed, record a share or a wait in AIRC.
- If your work depends on another PR, record a wait; continue and rebase when it merges.
- Message another agent only when a claim, overlap or dependency needs words. Never repeat events Velocity posts.
- Answer AIRC questions addressed to you. Reply to requests with done or declined and a reason. Never reply to inform messages.
- Log discoveries other agents need in the Discoveries ledger.

## Shared files
- Identify generated files and registration lists for this project here as they are established.
- Regenerate generated files after a merge. Never merge them by hand.
- Keep both additions to shared registration lists, inside each module's group.

## Sensitive paths
- PR duty gives a PR that touches a path listed here the sensitive review: full review, the full stress suite before merge, and a confidence proof at the strongest level its language allows.
- List one path, folder or glob per bullet, as inline code, followed by why it is sensitive. Add migrations, worktree deletion and auth code as they are established.
- `.github/workflows/`: CI config.

## Pull requests
- One piece, one branch, one PR. Name the branch `vel/<task-name>`.
- Use your thread's own worktree. Velocity creates and removes it; never create another worktree yourself.
- Describe what changed, why, the theorems proved and the Task List item or issue it completes.
- Opening the PR does not finish the task. Monitor it until it is merged.
- Fix failing checks on your change and push.
- Address every PR duty comment: fix and push, or reply with the reason. Resolve each thread and re-request review after fixes.
- Pull before every push; PR duty can update the branch. Never force-push.
- If the PR closes without merging, report why in your thread and keep the worktree.
- Never merge your own PR. Only the PR duty agent that approved a PR merges it.

## Review severity
- Block only data loss or corruption, a crash or hang, a security hole, a SPEC.md product-rule violation, a failing local check, or the claimed feature's main path failing its spec.
- File other findings as issues with evidence, link them in the review and approve.
- Put every blocking finding in the first review. Later rounds review new commits and block only findings in those commits or new data loss or a crash.
- When unsure whether a finding blocks, file an issue; do not block.

## Done means
- The piece meets SPEC.md and has been run successfully.
- The required local checks pass on the merge result and the required proofs check.
- The PR is merged and Velocity has removed its worktree.

## Never
- Never wait for a plan or tool approval. Run with full permissions and the provider's full toolset.
- Never start build work before the project spec is locked.
- Never push directly to the project's base branch.
- Never commit secrets, tokens or `.env` files.
- Never skip, delete or weaken a scout's failing script to make checks pass.
"#;
