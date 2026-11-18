<!-- split-spec: {"order":1470,"kind":"markdown","indentation":0} -->
### F16 · Goal swarm

**Rosen projects.**
Every member takes its own Rosen workspace from the project's warm pool, with git worktrees as the F5 fallback when Rosen cannot start.
The same native work choice and atomic AIRC piece lock apply to Rosen and git projects.
Rosen members keep their change identity when they claim a piece; they never create or rename a git branch, open a PR or start a PR watcher.
The goal dialog offers no PR duty capacity on a Rosen project.
Its locked context names the piece, tells the agent to claim files in Rosen, build and lint through `rosen run`, and finish by naming its work with `rosen commit`.

A named member change uses the ordinary Rosen landing path, including `review = "after-land"` (F7); goal swarms add no review or check gate.
The task is finished on `landed`.
That event ticks its tasks and records its built SPEC section, updates the board and activity feed, stops its session and watcher, releases its AIRC claim, and starts the next round in a fresh workspace when Keep going is on.
`reverted` restores the unfinished tasks and reopens the section only when no other merged thread built it, including deleted history (#376).
It keeps ownership with the original thread, updates the board and resumes that thread in its reopened workspace.
These state changes and the event cursor save together and survive restart.

A Rosen goal member's `airc_wait` on another member's claimed file becomes a stacked change (Rosen D19) after its session stops: Velocity snapshots its work, creates a child on the holder's unlanded change and continues the same task and transcript there.
The original change is retained until the replacement identity, workspace path and stack handoff are durable in state.json, including when saves are deferred.
A failed save retains the original; cleanup retries must pass the same save barrier before abandoning it through Rosen's deletion guard.
A copy that overlaps the dependency's edits, changes a nonregular file, or cannot be made safely keeps the original task waiting; on the holder's land it continues through Rosen's rebase, without a git checkout update.
A second dependency outside the current stack waits for its land.
The parent must land before the child; Rosen rebases its descendants.
The board and cards show named, fixing and landed changes, with a claim → named → landed track and landed-change counts.

Each Keep going round has a distinct workspace path, retaining its nick as the folder leaf.
A later revert can therefore reopen an earlier round without touching the next round's files.
Landed issue completions are saved locally, surviving history deletion, so Keep going cannot select an issue again while GitHub's mirror catches up.
A revert removes that completion and shows the original claim even if GitHub has already closed the issue.
The git lifecycle described below remains the fallback.

**What it is:**
- Your T3 Code routine as one action.
  Today you open 5–20 threads by hand and paste the same prompt into each: read AGENTS.md and start dev, push a branch, open a PR, watch it, keep the workspace on a given drive, remove it after the merge.
- A goal swarm starts all of those agents at once, with one prompt and the model mix you choose.
- Velocity picks work from the enabled inventory using the goal, the build order and the claims board.
  AIRC (F14) keeps two agents off the same piece.

**Dialog** (`G`, the "Goal swarm" starter chip, or the palette):
- **Prompt:** every agent gets exactly this text.
  The default is `Implement the next unclaimed piece.`; text typed in the empty composer prefills it.
- **Velocity adds to every agent** (locked):
  - its own workspace under the workspace root (F5), removed after its PR merges;
  - commit, push, open a PR against the base, and watch it until it lands (AGENTS.md);
  - join `#<project>` and claim work before editing, where the first claim wins;
  - implement and run first; then prove only features on the project's proof list (Rule 8, F17).

  These replace the lifecycle half of the hand-written prompt.
  The workspace drive comes from Settings → Workspaces, not from the prompt.
- **Agents:** a stepper from 1 to 30, with quick picks 3 / 5 / 10 / 20.
  All of them start at once.
- **Mix:** one row per model.
  - Each row has a model picker (signed-in providers only, F15), effort (Low / Med / High / Max), a Fast toggle (greyed out when the model has no fast mode) and a count.
  - The first row is "the rest".
    It absorbs the difference, so the rows always add up to the agent count.
  - Raising another row takes one agent from the rest; if the rest is empty, it raises the total.
  - Buttons: **Add a model** and **Even split**.
  - A bar above the rows shows the split in provider colours.
  - Next to the buttons is a preview of the nicks, for example `sol-1…6 · argon-1…2 · fable-1…2`.
  - Example: 10 agents = 6 GPT-6.1 Sol High + 2 Gemini 4 Argon High + 2 Fable 5.1 High.
- **Limits:** one line per provider in the mix: agent count, share of the window used, and the reset time when 60% or more is used (F15).
- **Work sources:** independent Task List, Issues and Unbuilt spec sections switches, all on by default.
  At least one must be on.
  The choices are saved with the swarm; + Agents and every Keep going round inherit them.
- **Pool:** "They choose from N unclaimed pieces: x Task List · y issues · z unbuilt SPEC.md sections."
  If there are more agents than pieces, it says how many will find nothing and leave.
- **Options:**
  - **Keep going** (off by default): after its PR merges, an agent claims the next unclaimed piece in a fresh workspace instead of stopping.
  - **PR duty capacity:** the same check as the Swarm dialog (one reviewer per 3 open PRs).
- **Launch N agents** (`Ctrl+Enter`).

**Dialog state.**
The window holds only the choices (the agent count and the mix rows) and sends them back with one edit.
Rust applies the edit, restores the "rest" rule and draws the dialog again.
- **Models:** until Settings → Providers lists each provider's models (F15), the picker offers one model per launchable provider, as the Swarm dialog does.
  Fast stays selectable; a session on a model without a fast mode runs at Regular.
- **Nicks:** a model's short name is the last plain word of its name, in lower case (`sol`, `argon`, `fable`).
  The preview numbers on from the highest number that name has used in the project.
- **Limits:** a line carries its provider's agent count, and adds the share used and the reset time once the provider reports them (Q14).
- **Pool and PR duty:** both use the work pool read when the dialog opens.
  PR duty is sized for the PRs the launch can open: the smaller of the agent count and the unclaimed pieces, plus the PRs already open.

**What each agent does:**
1.
   Gets a nick such as `sol-3`, a workspace named after it and the project's session instructions (§3.2).
   State: **choosing work**.
2.
   Velocity picks unclaimed work in §7 order, followed by Task List order and then issues by creation time, oldest first.
   It skips pieces whose known files overlap held claims and, when the code graph is available, pieces in held subsystems (F19).
   A custom goal that requires interpretation goes to the implementation session in its first turn with up to five candidates ranked by relevance.
   The session returns its acceptable candidates in preference order and waits for the claimed piece before editing.
   Velocity validates that choice against the shortlist.
   A failed or malformed choice reports an error, and an empty choice leaves without a claim.
   Only enabled sources contribute to the pool:
   - open Task List items, except drafts and items another agent is already on;
   - open issues, except owner-decision questions and `release-blocker` issues, using the same eligibility rule as continuous dispatch (F8);
   - SPEC.md sections nobody has built.
3.
   Claims it with `airc_claim`.
   - **Granted:** it renames its branch for the piece (`vel/<slug>`, or `vel/issue-<n>`), posts its claim with files, and continues.
   - **Refused** (another agent claimed the piece or its files after Velocity read the board): a `refused` event names the holder.
     Velocity checks the claims again and retries the next available candidate in native code without another model call.
   - **Nothing left:** a `left` event is posted; the agent removes its workspace and leaves.
4.
   From here it is an ordinary thread (F4–F6).
   Velocity starts the implementation session, or continues the same session that interpreted the goal.
   Its implementation prompt names the chosen spec section when known and directs the member to read applicable headings with `graph_read`.
   It settles overlaps and dependency waits in AIRC (F14), opens a PR, watches it until PR duty merges it, and its worktree is deleted.
   Merging ticks the task, closes the issue, or records the SPEC.md section as built in the project's state.
   Velocity records it at landing; the agent's PR never edits SPEC.md to mark it, so changes don't all touch SPEC.md.
5.
   With keep going on, the same nick goes back to step 2.

Agents launched together read the board within seconds of each other, so several often want the same piece.
The lock is what makes that safe.
The losers advance to the next candidate without duplicating work.

**Work inventory.**
A goal swarm reads only its enabled sources, including its pool preview and board, so a failure in a disabled source cannot prevent choosing work.
Counts and PR duty capacity use the enabled pool.
The project-wide `get_work_pool` inventory continues to include every source.
`get_work_pool` and the `velocity-threads` `work_pool` example read the same project-scoped inventory.
Draft/done tasks, ownerless now tasks, closed issues, owner-decision questions (including unlabelled decision requests) and `release-blocker` issues are omitted using the continuous-dispatch eligibility rule (F8); owned work stays visible but does not count as available.
Failed/closed owners retain their pieces.
Unlocked projects return an empty pool.
SPEC.md's Implementation order/Implementation plan checklist supplies unfinished entries when present; otherwise checklist-bearing Markdown sections supply one piece each.
Fenced examples are ignored.
Spec identities use heading context and entry text rather than line offsets.
A thread's defaulted `spec_section` identity retains ownership and records completion when merged, without editing SPEC.md; deleting merged history retains the identity in the project's defaulted `built_spec_sections`.
Completion evidence from deleted merged threads is saved separately with the deletion, so reverting a different thread cannot erase it, including after a restart (#376).
Selection, atomic claims and launch remain separate §7 items.

For a split spec (F2), the implementation-plan preference applies across all source files.
Each file is parsed independently, so headings and fenced examples do not leak into the next file.
Without a plan, a checklist-bearing section uses its nearest heading ID (F5, §3.1 or R8) as its identity, falling back to heading context for unnumbered sections, and its source path as its reference.
Single-file projects retain their existing work identities.

**Sidebar:** one row under Swarms, with a target icon, "Goal · <prompt>", ×<agents> and the segment bar.
Its members nest under it like swarm members, and each member is titled by the piece it claimed.

**Goal swarm view:**
- **Header:** title, "<n> agents · one prompt", start time, changes merged.
  Buttons: **AIRC** and **+ Agents** (more agents on the same prompt).
- **Prompt bubble:** "goal swarm · the same prompt to all <n> · <mix>".
- **Segment bar:** one segment per agent (choosing, working, named, addressing review, merged, failed), plus "<n> waiting on another agent's PR".
- **Claims board:** every piece in the pool, in three groups:
  - claimed pieces first, with the holder's nick and state (working, "waits for <nick> · #n", "#n · in review", failed · claim held);
  - open pieces next;
  - pieces this swarm merged last, struck through, with "merged #n".
- **Agents:** one card per agent showing nick, model, effort (and "Fast" when on), claimed piece, the claim → PR → CI → review → merged track, and what it is doing.
  Clicking a card opens that thread.
- **Activity:** claims, refusals, changes, waits, merges and leaves.
- **Composer:** messages every agent in the swarm (read at its next step, Rule 1).

**Acceptance:**
- One action starts N agents with the exact mix and efforts chosen.
  Each runs in its own workspace on the configured drive.
- The default goal starts one implementation session after native work selection.
  A custom goal uses the implementation session's first turn to evaluate up to five candidates.
  Tasks-only and issues-only choices exclude every other source from selection and the board.
- No two agents ever hold a claim on the same piece.
  A refused claim shows in AIRC, and Velocity tries the next candidate without another model call.
- Every agent's work ends in a merged PR (or a held claim on failure), and its workspace is deleted on merge.
- With more agents than pieces, the extras leave without making a workspace change or a PR.
- Keep going: an agent that finishes takes the next open piece until the pool is empty.

---
