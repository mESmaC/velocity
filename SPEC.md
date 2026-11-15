# SPEC.md · Velocity

Velocity is a desktop agentic IDE built for one goal: finish projects as fast as possible by running many coding agents in parallel, without interruptions. Built in Rust + Tauri.

- **Status:** draft. Written from the design mockup (`design/main-window.html`) and the decisions recorded in it, 2026-11-07.
- **Reference UI:** `design/main-window.html`. Open it in a browser; every feature below is mocked there with live simulated data.
- **How to read this:** §1 holds the rules that override everything else. §4 describes each feature, ending in a list of acceptance criteria. §7 is the build order. §8 lists the questions still open: do not build anything that depends on them until they are answered.
- **Marker:** items marked *(proposed)* came from the mockup and have not been confirmed yet. Everything else is decided.

---

## 1. Rules

These hold for every feature. A feature that breaks one of them is wrong, whatever else this spec says.

1. **Agents never wait for approval.** Every agent runs with full permissions (bypass / yolo / unrestricted) until it finishes. Every agent also has its full toolset: all of its provider's built-in tools, your MCP servers and skills, the shell, the network and GitHub (§3.2 Tool access). Velocity has no approval prompts, no plan-approval gates and no "needs you" states. Messages sent to a busy agent are queued and picked up at its next step. They never interrupt it.
2. **The spec comes first.** Every new project starts with a spec interview. No build work starts until the spec is locked.
3. **A task is finished when its PR is merged, not when the code is written.** An agent opens a PR, then watches it until it merges. It fixes CI and answers every review comment as it arrives.
   - **On a Rosen project** (F5, Rosen rules below): a task is finished when its change lands. The agent names its work (`rosen commit -m`), and fixes what a conflict or a post-land revert sends back to it.
4. **Never merge without approval.** Only a PR duty agent that approved a PR merges it.
   - **On a Rosen project:** no approval gates a land. A named change lands as soon as it merges cleanly onto the base; the fast build and lint run right after the land and revert a change that breaks them, and the full tests follow and file a failure as an issue (F7 "A Rosen change"). Review happens after the land, by agents that review, scout and verify landed code without blocking development (#382). The release is the gate (F7 "The release gate").
   - PR landing with local checks uses the live head and base branch targets when GitHub's cached PR record lags; checks and approval stay bound to the exact live head (F6, #350).
5. **One worktree per unit of work.** Every thread and every agent works in its own git worktree. The worktree is deleted as soon as that work's PR merges. PR duty also removes its review worktree after a confirmed merge by someone else, once its review session has stopped (F7).
   - **On a Rosen project:** one Rosen workspace per unit of work. Every thread and every agent works in its own Rosen change and workspace, which Rosen deletes when the change lands or is abandoned.
6. **AGENTS.md is direct and explicit.** Hedged or vague wording counts as a defect.
7. **Agents coordinate in AIRC through claims, not conversation.** Every agent claims its work and files, records a share or a wait when it needs a file another agent claimed, and waits for a PR it depends on to merge. Agents can message each other, but only when a claim, overlap or dependency needs words. A dependency wait is not an approval gate (Rule 1).
8. **Implementation first.** Agents build working functionality before anything else. Features on the project's proof list are proven in Lean after their code, in the same PR (F17); everything else ships with no Lean. Tests and docs are low priority: a building agent adds a test only for a critical contract, and scouts and PR duty write the adversarial tests. Only the agents whose job is to break things write tests and stress scripts: scouts and PR duty. Lean is the proving ground, not the test suite.
   - **On a Rosen project:** Lean for a proof-list feature follows its land, written and checked by proof agents. It no longer has to be in the same change. The release gate's `lake build` and axiom audit hold every release to it.

**Rosen rules** *(decided, #381; the project owner, 2026-11-14)*: "With rosen, pr agents become basically obsolete. They existed to help settle github work faster and cleaner. But with rosen, we care about speed. Then we can have agents that can swarm the project to look for issues and review the code and implementations, use lean to verify implementations, and so on, without blocking development."
- A Rosen project here is one whose `rosen.toml` sets `review = "after-land"` (Rosen R7 and R8, rosen#365; Velocity #380). A git project keeps the wording above, and so does a Rosen project that keeps `review = "before-land"`, Rosen's default: there PR duty still reviews and Rosen still lands nothing without its approval and a receipt for its pre-land checks (F7 "A Rosen change").
- What stays: no work is lost (Rosen's durability, and a revert never discards the author's work), every land's build and lint are verified with bisection and the automatic revert of a broken build, the full tests run after the land, and git projects work as before.
- A failing or rejected Rosen mirror stays visible on its project until the next successful push; F5 specifies the project header, Settings, retry time and rejection notice (#402).
- A build culprit whose revert is refused, or remains unreverted for two minutes, gets one fix-forward thread on the default implementation model; F5/F7 specify its evidence, scope and closure on a passing post-land build (#406).
- Discoveries can report problems in either Velocity or Rosen to their own repositories, with separate title matching and filing results (F14, #303).
- Generated lockfiles and bindings are regenerated rather than line-merged; F5 specifies the commands in durable rebase notices, including rebases with no text conflicts (#404).
- A Rosen project never falls back to a git worktree when workspace creation fails: transient errors retry Rosen for about a minute, and a lasting failure leaves the thread in `error` with the Rosen error and a retry action (F5, #407).
- Rosen AIRC record acknowledgements and dependency subscriptions replay from an explicit cursor so simultaneous Goal Swarm setup cannot miss its own events; record event waits fail after 30 seconds (F14, #409).
- Switching from Rosen to git carries Rosen's landed base into the default branch before new worktrees or ready-pool slots start; divergent history is refused with an explanation (F5, #369).

---

## 2. Concepts

| Term | Meaning |
|---|---|
| **Project** | A local folder or GitHub repo. It owns threads, a Task List, PRs, issues, AGENTS.md and SPEC.md. Velocity holds several projects and you switch between them. |
| **Thread** | One agent working on one piece of work, in its own worktree. |
| **Spec interview** | The first thread of a new project. It interviews you and produces SPEC.md plus the first Task List. |
| **Task List** | The project-level list of work, shown in the top-right panel. It is never a per-thread plan. |
| **Duty agent** | A long-running agent with a standing role: **PR duty**, **Scout**, or the **Review swarm** on after-land Rosen projects (F7). Names are *(proposed)*. |
| **Swarm** | One agent per open item (tasks, issues, or both), all started at once. |
| **Goal swarm** | N agents given one identical prompt (e.g. "Read AGENTS.md and start dev.") with a model mix and work sources you set. Each agent reads the prompt before choosing a matching piece from the enabled sources and claims it atomically in AIRC. |
| **Worktree** | A `git worktree` checkout made for one thread or one agent's job from the project's default branch, falling back to `master` when none is set; its transcript setup labels name that base branch. |
| **AIRC** | The project's coordination channel. It is first a log of claims and project events (PRs, reviews, merges, issues, Task List changes); agents can also message each other. Overlaps and dependencies are settled there and shared ledgers are kept there, so no agent works in isolation. Modelled on xros_chat. |
| **Claim** | An agent's public statement of its task, branch and the files it expects to touch. Claims on a piece of work are locked: the first claim wins and a later one is refused with the holder's nick. |
| **Design proof** | `proofs/<Feature>/Design.lean`: the feature modelled in Lean, proven correct, within a stated cost bound, and no worse than a stated class of alternatives. Only for features on the proof list (F17), written after the code in the same PR. |
| **Confidence proof** | Ties what was built to the design model. By default, the model exports a fixture table and one test checks the code against it; a code translation runs only when an issue asks for it. "Don't state your confidence; prove it." |
| **Proof level** | How firmly a language's code is tied to the proven model: **code** (the code itself is translated into Lean and proven), **reference** (the code is stress-tested against the compiled Lean model) or **model** (only the design is proven). |
| **Skill** | An Agent Skills folder (`SKILL.md` plus optional scripts, references and data) that an agent loads when a task needs it. Velocity installs skill packs once and turns them on per project (F18). |
| **Evidence** | The failing test or stress script a scout attaches to every issue it files. |

### 2.1 Thread states

| State | Sidebar group | Meaning |
|---|---|---|
| `draft` | (top, unnamed) | New thread, not sent yet. |
| `spec` | Spec interview | Interview in progress. Phases: `scan` → `ask` → `ready` → `locked`. |
| `working` | Running | Agent works in its worktree; an idle conversation can resume on the next message (§3.2). |
| `review` | PR open · watching | PR is open; the agent watches it. Stages: `ci`, `review`, `fixing`, `approved`. |
| `error` | Failed | Agent stopped on an error. Its worktree is kept. |
| `closed` | Settled | PR closed without merging. Its worktree is kept. |
| `done` | Settled | Spec locked (spec threads only). |
| `merged` | Settled | PR merged and worktree deleted. The thread is finished. |

The sidebar shows groups in this order: draft, On duty, Swarms, Spec interview, Running, PR open · watching, Failed, Settled. Settled is collapsed by default, shows its count and a chevron, and is the only collapsible group. Its expanded state is saved per project. Finished swarm members move there immediately; idle working threads and failures stay in their active groups. Search reveals matching settled threads even while collapsed. Empty groups are hidden, except "On duty", which shows a "start one" button when empty.

Command errors clear after four seconds or the next successful dispatch. Key reports (`Keyboard`, `NumberHints`) don't count as a dispatch here: Enter's own key report would otherwise hide a failed send's error at once. Backend launch notices remain visible while set.

### 2.2 Agent activity

A thread's state (§2.1) says where it is in its lifecycle. Its **activity** says what its agent is doing right now. The two are tracked separately, as Google Scion does. Velocity sets activity from the ACP stream (§3.2) and from AIRC (F14).

| Activity | Meaning | Set by |
|---|---|---|
| `thinking` | The agent is producing output. | ACP message chunks |
| `executing` | A tool call is running. | ACP `tool_call` |
| `parked` | The agent is waiting on another agent's PR (`airc_wait`). Its ACP session is ended to free a provider slot, and its worktree and claims are kept. | `airc_wait` |
| `stalled` | No ACP update for the stall timeout (below) while not parked. | Velocity |
| `at limit` | The agent hit a per-agent cap or its provider's subscription limit. This stays set until the agent resumes. | Velocity, provider |
| `idle` | The agent's turn ended and nothing is queued. | ACP turn end |

**Parked agents wake on events, not by polling.** A wait subscribes the agent to the `merged` event of the PR it waits on. When that event fires, Velocity resumes the session (`session/load`, or a new session with the transcript if the agent can't load one), rebases, and posts `unblocked`. If the PR is closed, fails or is deleted, the agent wakes and the wait ends. A dependency wait also ends after 20 minutes, including across app restarts; the resumed agent is told the wait expired and to coordinate with the holder or proceed. Expiry does not rebase or release either claim.

**Stall timeout** (decided, Q20): 5 minutes without an ACP update. Two cases never count as silence:
- **A running tool call** gets 30 minutes *(proposed)*. A build, a test run or `lake build` reports nothing until it ends, and restarting the session would kill it and start it again.
- **The build queue** (F5): an agent waiting in the queue, or running the build at its head, is never stalled.

Both timeouts are set in Settings → Agents (F13).

**Stall handling:**
1. Velocity sends one prompt: "No output for 5 minutes. Continue, or say what is blocking you."
2. If there is still no update after another 5 minutes, Velocity cancels the turn and restarts the session in the same worktree from its transcript. The agent keeps its claims.
3. Each step posts a `stalled` event.

**Limits and claim release:**
- **Per-agent caps:** max turns and max duration, set in Settings → Agents (F13). Off by default.
- **Hitting a limit:** when an agent hits a cap or its provider's limit, its activity becomes `at limit`.
- **Release after a grace period:** if an agent stays `at limit`, or its thread has failed (§2.1 `error`), for 10 minutes (decided, Q20; set in Settings → Agents), Velocity releases its claims. It posts a `released` event and the piece goes back to the pool (F16). The worktree is still kept (F5).
- **An open PR keeps its claims** *(proposed)*. A thread with an open PR is never released. PR duty can still land that PR, and a second agent on the piece would open a duplicate.
- **Prior work is offered, never assigned** (decided, Q20). When the released agent pushed a branch, the `released` event names the branch and its last pushed commit. The work pool shows them on the piece, and the agent that claims it next is told "<nick> left work on `<branch>` at `<sha7>`". That agent starts in its own fresh worktree from `master` as always (Rule 5) and decides whether to merge the branch in. The released agent's worktree stays with its thread.
- **At a provider's limit, the agent waits for the reset** (decided, Q15), as T3 Code does. Velocity never moves a thread, queued work or new work to another provider. The thread shows the window that ran out and when it resets, and Velocity resumes the session at the reset with no action from you. When the provider reports no reset time, Velocity retries every 15 minutes *(proposed)*. You can change the thread's model by hand at any time (F4). The piece itself still moves on: the grace period above returns it to the pool, where an agent on any provider can claim it.
- **Resuming after a release** *(proposed)*: an agent whose claims were released claims its piece again before it continues. If another agent holds the piece by then, the thread fails with "piece taken by <nick>" and keeps its worktree.
- **Closed PRs:** a closed PR keeps its claims until you delete the thread. Closing is a decision someone made, so it isn't treated as a failure.

The native supervisor checks live build threads once a second. Its defaulted checkpoint in `state.json` retains completed turns, the session start, the last ACP update at a durable transition, the nudge, and the unavailability grace deadline. The exact live ACP heartbeat stays in memory; streaming batches do not write supervision checkpoints. Streaming batches save the live transcript to `state.json` at most once every 5 seconds per thread, and always at the end of a turn, so copying every chunk of every agent into the store no longer starves the window (#273). A change to saved state only updates memory; a background writer saves the newest state to `state.json` at most once a second, copying it under the store lock and serializing and writing it outside the lock, as compact JSON, and once more as Velocity exits or hands over to an installer (#285, #320). A crash loses at most the last second of changes, and mid-turn at most those last seconds of transcript. Session start, turn end, nudge and the first subsequent ACP update persist checkpoints; a resumed session starts a fresh silence clock. Recovery stops the owned ACP session and resumes in the registered checkout through the existing load/transcript path. Caps end the session; raising or disabling the cap resumes it. A usage-limit turn error retains the provider and retries after its reported reset, or every 15 minutes when none is reported. Released Task List ownership is returned to todo; issues and SPEC pieces become available again. Optional prior pushed work is shown in the pool. Resumption must win the atomic claim again before starting ACP, and fails with the holder named if the piece was taken.

**Sidebar and views:** `stalled` and `at limit` show in amber in the thread's sidebar meta and on its swarm or goal swarm card. `parked` shows "waiting for <nick> · #n".

---

<!-- split-spec index -->

## Spec file index

Velocity reads this file and Markdown recursively under `spec/` as one spec.
Section IDs (including F5, F7 and §3.1) are unchanged.
§7 is in `spec/7-implementation-order.md`; §8 is in `spec/8-open-questions.md`.
F5, F7 and §3.1 have a README for their introduction and separate files for their rules or each feature's IPC.

- [spec/3-architecture.md](spec/3-architecture.md)
- [spec/3.1-ipc/README.md](spec/3.1-ipc/README.md)
- [spec/3.1-ipc/F1.md](spec/3.1-ipc/F1.md)
- [spec/3.1-ipc/F2.md](spec/3.1-ipc/F2.md)
- [spec/3.1-ipc/F4.md](spec/3.1-ipc/F4.md)
- [spec/3.1-ipc/F6.md](spec/3.1-ipc/F6.md)
- [spec/3.1-ipc/F12.md](spec/3.1-ipc/F12.md)
- [spec/3.1-ipc/F3.md](spec/3.1-ipc/F3.md)
- [spec/3.1-ipc/F7.md](spec/3.1-ipc/F7.md)
- [spec/3.1-ipc/F9.md](spec/3.1-ipc/F9.md)
- [spec/3.1-ipc/F10.md](spec/3.1-ipc/F10.md)
- [spec/3.1-ipc/F8.md](spec/3.1-ipc/F8.md)
- [spec/3.1-ipc/F16.md](spec/3.1-ipc/F16.md)
- [spec/3.1-ipc/F11.md](spec/3.1-ipc/F11.md)
- [spec/3.1-ipc/F13.md](spec/3.1-ipc/F13.md)
- [spec/3.1-ipc/F5.md](spec/3.1-ipc/F5.md)
- [spec/3.1-ipc/F15.md](spec/3.1-ipc/F15.md)
- [spec/3.1-ipc/3.2.md](spec/3.1-ipc/3.2.md)
- [spec/3.1-ipc/F19.md](spec/3.1-ipc/F19.md)
- [spec/3.1-ipc/F17.md](spec/3.1-ipc/F17.md)
- [spec/3.1-ipc/F18.md](spec/3.1-ipc/F18.md)
- [spec/3.1-ipc/F14.md](spec/3.1-ipc/F14.md)
- [spec/3.2-agent-protocol-acp.md](spec/3.2-agent-protocol-acp.md)
- [spec/3.3-prior-art-scion.md](spec/3.3-prior-art-scion.md)
- [spec/4-features.md](spec/4-features.md)
- [spec/F1-main-window.md](spec/F1-main-window.md)
- [spec/F2-new-project.md](spec/F2-new-project.md)
- [spec/F3-spec-interview.md](spec/F3-spec-interview.md)
- [spec/F4-threads-and-composer.md](spec/F4-threads-and-composer.md)
- [spec/F5-worktrees/README.md](spec/F5-worktrees/README.md)
- [spec/F5-worktrees/headless-machines.md](spec/F5-worktrees/headless-machines.md)
- [spec/F5-worktrees/rosen-the-project-s-server.md](spec/F5-worktrees/rosen-the-project-s-server.md)
- [spec/F5-worktrees/rosen-a-thread-s-workspace.md](spec/F5-worktrees/rosen-a-thread-s-workspace.md)
- [spec/F5-worktrees/rosen-stored-as-the-agent-writes.md](spec/F5-worktrees/rosen-stored-as-the-agent-writes.md)
- [spec/F5-worktrees/rosen-what-the-agent-is-told.md](spec/F5-worktrees/rosen-what-the-agent-is-told.md)
- [spec/F5-worktrees/rosen-deleting-the-thread.md](spec/F5-worktrees/rosen-deleting-the-thread.md)
- [spec/F5-worktrees/rosen-followed-through-rosen-s-events.md](spec/F5-worktrees/rosen-followed-through-rosen-s-events.md)
- [spec/F5-worktrees/rosen-issues-carried-by-rosen-changes.md](spec/F5-worktrees/rosen-issues-carried-by-rosen-changes.md)
- [spec/F5-worktrees/rosen-rebases-at-the-step-boundary.md](spec/F5-worktrees/rosen-rebases-at-the-step-boundary.md)
- [spec/F5-worktrees/rosen-rebases-at-the-step-boundary-airc-rebase-tool.md](spec/F5-worktrees/rosen-rebases-at-the-step-boundary-airc-rebase-tool.md)
- [spec/F5-worktrees/rosen-rebases-at-the-step-boundary-a-conflict.md](spec/F5-worktrees/rosen-rebases-at-the-step-boundary-a-conflict.md)
- [spec/F5-worktrees/rosen-falling-back.md](spec/F5-worktrees/rosen-falling-back.md)
- [spec/F5-worktrees/lifecycle.md](spec/F5-worktrees/lifecycle.md)
- [spec/F5-worktrees/path.md](spec/F5-worktrees/path.md)
- [spec/F5-worktrees/settings-worktrees.md](spec/F5-worktrees/settings-worktrees.md)
- [spec/F5-worktrees/settings-worktrees-location.md](spec/F5-worktrees/settings-worktrees-location.md)
- [spec/F5-worktrees/settings-worktrees-location-a-drive-picker-showing-each-drive.md](spec/F5-worktrees/settings-worktrees-location-a-drive-picker-showing-each-drive.md)
- [spec/F5-worktrees/settings-worktrees-location-a-folder-field-with-browse.md](spec/F5-worktrees/settings-worktrees-location-a-folder-field-with-browse.md)
- [spec/F5-worktrees/settings-worktrees-location-a-live-path-preview.md](spec/F5-worktrees/settings-worktrees-location-a-live-path-preview.md)
- [spec/F5-worktrees/settings-worktrees-location-a-per-project-override.md](spec/F5-worktrees/settings-worktrees-location-a-per-project-override.md)
- [spec/F5-worktrees/settings-worktrees-location-a-notice-when-the-location-changed.md](spec/F5-worktrees/settings-worktrees-location-a-notice-when-the-location-changed.md)
- [spec/F5-worktrees/settings-worktrees-lifecycle.md](spec/F5-worktrees/settings-worktrees-lifecycle.md)
- [spec/F5-worktrees/settings-worktrees-new-worktree-setup.md](spec/F5-worktrees/settings-worktrees-new-worktree-setup.md)
- [spec/F5-worktrees/settings-worktrees-new-worktree-setup-untracked-files-to-copy.md](spec/F5-worktrees/settings-worktrees-new-worktree-setup-untracked-files-to-copy.md)
- [spec/F5-worktrees/settings-worktrees-new-worktree-setup-a-command-to-run-after-creation.md](spec/F5-worktrees/settings-worktrees-new-worktree-setup-a-command-to-run-after-creation.md)
- [spec/F5-worktrees/settings-worktrees-new-worktree-setup-share-the-rust-build-cache-between.md](spec/F5-worktrees/settings-worktrees-new-worktree-setup-share-the-rust-build-cache-between.md)
- [spec/F5-worktrees/settings-worktrees-live-worktrees.md](spec/F5-worktrees/settings-worktrees-live-worktrees.md)
- [spec/F5-worktrees/warm-pool.md](spec/F5-worktrees/warm-pool.md)
- [spec/F5-worktrees/warm-pool-one-base-clone.md](spec/F5-worktrees/warm-pool-one-base-clone.md)
- [spec/F5-worktrees/warm-pool-ready-made-worktrees.md](spec/F5-worktrees/warm-pool-ready-made-worktrees.md)
- [spec/F5-worktrees/warm-pool-ready-made-worktrees-a-ready-worktree-is-a-detached.md](spec/F5-worktrees/warm-pool-ready-made-worktrees-a-ready-worktree-is-a-detached.md)
- [spec/F5-worktrees/warm-pool-ready-made-worktrees-the-setup-command-runs-after-the.md](spec/F5-worktrees/warm-pool-ready-made-worktrees-the-setup-command-runs-after-the.md)
- [spec/F5-worktrees/warm-pool-taking-one.md](spec/F5-worktrees/warm-pool-taking-one.md)
- [spec/F5-worktrees/warm-pool-refilling.md](spec/F5-worktrees/warm-pool-refilling.md)
- [spec/F5-worktrees/warm-pool-refilling-built.md](spec/F5-worktrees/warm-pool-refilling-built.md)
- [spec/F5-worktrees/warm-pool-skills.md](spec/F5-worktrees/warm-pool-skills.md)
- [spec/F5-worktrees/warm-pool-lean.md](spec/F5-worktrees/warm-pool-lean.md)
- [spec/F5-worktrees/warm-pool-lean-built.md](spec/F5-worktrees/warm-pool-lean-built.md)
- [spec/F5-worktrees/warm-pool-swarm-launches.md](spec/F5-worktrees/warm-pool-swarm-launches.md)
- [spec/F5-worktrees/warm-pool-build-queue.md](spec/F5-worktrees/warm-pool-build-queue.md)
- [spec/F5-worktrees/warm-pool-build-queue-the-run-queue-s-classes-overlap.md](spec/F5-worktrees/warm-pool-build-queue-the-run-queue-s-classes-overlap.md)
- [spec/F5-worktrees/guards.md](spec/F5-worktrees/guards.md)
- [spec/F5-worktrees/guards-velocity-is-the-only-thing-that.md](spec/F5-worktrees/guards-velocity-is-the-only-thing-that.md)
- [spec/F5-worktrees/guards-the-pool-removes-only-its-own.md](spec/F5-worktrees/guards-the-pool-removes-only-its-own.md)
- [spec/F5-worktrees/guards-deleting-a-worktree-removes-only-that.md](spec/F5-worktrees/guards-deleting-a-worktree-removes-only-that.md)
- [spec/F5-worktrees/guards-worktree-folder-names-are-unique-per.md](spec/F5-worktrees/guards-worktree-folder-names-are-unique-per.md)
- [spec/F5-worktrees/guards-a-slug-made-from-a-title.md](spec/F5-worktrees/guards-a-slug-made-from-a-title.md)
- [spec/F5-worktrees/accept-when.md](spec/F5-worktrees/accept-when.md)
- [spec/F5-worktrees/accept-when-with-a-ready-worktree-in-the.md](spec/F5-worktrees/accept-when-with-a-ready-worktree-in-the.md)
- [spec/F5-worktrees/accept-when-two-agents-never-share-a-worktree.md](spec/F5-worktrees/accept-when-two-agents-never-share-a-worktree.md)
- [spec/F5-worktrees/accept-when-every-merge-deletes-its-worktree-within.md](spec/F5-worktrees/accept-when-every-merge-deletes-its-worktree-within.md)
- [spec/F5-worktrees/accept-when-closed-and-failed-threads-keep-their.md](spec/F5-worktrees/accept-when-closed-and-failed-threads-keep-their.md)
- [spec/F5-worktrees/accept-when-changing-the-location-never-moves-or.md](spec/F5-worktrees/accept-when-changing-the-location-never-moves-or.md)
- [spec/F5-worktrees/accept-when-sharing-the-build-cache-uses-one.md](spec/F5-worktrees/accept-when-sharing-the-build-cache-uses-one.md)
- [spec/F5-worktrees/accept-when-two-agents-heavy-builds-never-run.md](spec/F5-worktrees/accept-when-two-agents-heavy-builds-never-run.md)
- [spec/F6-pr-follow-through.md](spec/F6-pr-follow-through.md)
- [spec/F7-duty-agents/README.md](spec/F7-duty-agents/README.md)
- [spec/F7-duty-agents/duty-view.md](spec/F7-duty-agents/duty-view.md)
- [spec/F7-duty-agents/duty-view-rules.md](spec/F7-duty-agents/duty-view-rules.md)
- [spec/F7-duty-agents/duty-view-agents.md](spec/F7-duty-agents/duty-view-agents.md)
- [spec/F7-duty-agents/duty-view-activity.md](spec/F7-duty-agents/duty-view-activity.md)
- [spec/F7-duty-agents/duty-view-header-actions.md](spec/F7-duty-agents/duty-view-header-actions.md)
- [spec/F7-duty-agents/duty-view-composer.md](spec/F7-duty-agents/duty-view-composer.md)
- [spec/F7-duty-agents/duty-view-dismiss-and-stop.md](spec/F7-duty-agents/duty-view-dismiss-and-stop.md)
- [spec/F7-duty-agents/velocity-s-fast-build-set.md](spec/F7-duty-agents/velocity-s-fast-build-set.md)
- [spec/F7-duty-agents/measured-check-times.md](spec/F7-duty-agents/measured-check-times.md)
- [spec/F7-duty-agents/pr-duty.md](spec/F7-duty-agents/pr-duty.md)
- [spec/F7-duty-agents/pr-duty-for-git-projects.md](spec/F7-duty-agents/pr-duty-for-git-projects.md)
- [spec/F7-duty-agents/pr-duty-claims-unclaimed-prs-one-pr-per.md](spec/F7-duty-agents/pr-duty-claims-unclaimed-prs-one-pr-per.md)
- [spec/F7-duty-agents/pr-duty-reviews-the-diff-against-spec-md.md](spec/F7-duty-agents/pr-duty-reviews-the-diff-against-spec-md.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge.md](spec/F7-duty-agents/pr-duty-checks-before-merge.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-review-by-reading.md](spec/F7-duty-agents/pr-duty-checks-before-merge-review-by-reading.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head.md](spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head-checks-still-running.md](spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head-checks-still-running.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head-a-failed-check.md](spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head-a-failed-check.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head-a-conflict-with-the-base-branch.md](spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head-a-conflict-with-the-base-branch.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head-an-open-review-thread.md](spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head-an-open-review-thread.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head-the-merge-is-the-approver-s.md](spec/F7-duty-agents/pr-duty-checks-before-merge-velocity-lands-the-approved-head-the-merge-is-the-approver-s.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-local-checks.md](spec/F7-duty-agents/pr-duty-checks-before-merge-local-checks.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-an-approved-head-whose-github-checks.md](spec/F7-duty-agents/pr-duty-checks-before-merge-an-approved-head-whose-github-checks.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-with-the-checks-pass-rule-off.md](spec/F7-duty-agents/pr-duty-checks-before-merge-with-the-checks-pass-rule-off.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-why.md](spec/F7-duty-agents/pr-duty-checks-before-merge-why.md)
- [spec/F7-duty-agents/pr-duty-checks-before-merge-what-it-gives-up.md](spec/F7-duty-agents/pr-duty-checks-before-merge-what-it-gives-up.md)
- [spec/F7-duty-agents/pr-duty-rules.md](spec/F7-duty-agents/pr-duty-rules.md)
- [spec/F7-duty-agents/pr-duty-rules-never-merge-without-an-approving-review.md](spec/F7-duty-agents/pr-duty-rules-never-merge-without-an-approving-review.md)
- [spec/F7-duty-agents/pr-duty-rules-never-merge-unless-the-proofs-check.md](spec/F7-duty-agents/pr-duty-rules-never-merge-unless-the-proofs-check.md)
- [spec/F7-duty-agents/pr-duty-rules-review-every-theorem-statement-against-the.md](spec/F7-duty-agents/pr-duty-rules-review-every-theorem-statement-against-the.md)
- [spec/F7-duty-agents/pr-duty-rules-checks-pass-before-merge.md](spec/F7-duty-agents/pr-duty-rules-checks-pass-before-merge.md)
- [spec/F7-duty-agents/pr-duty-rules-check-against-spec-md.md](spec/F7-duty-agents/pr-duty-rules-check-against-spec-md.md)
- [spec/F7-duty-agents/pr-duty-rules-re-review-on-new-commits.md](spec/F7-duty-agents/pr-duty-rules-re-review-on-new-commits.md)
- [spec/F7-duty-agents/pr-duty-rules-update-branches-with-master.md](spec/F7-duty-agents/pr-duty-rules-update-branches-with-master.md)
- [spec/F7-duty-agents/pr-duty-rules-delete-the-branch-after-merge.md](spec/F7-duty-agents/pr-duty-rules-delete-the-branch-after-merge.md)
- [spec/F7-duty-agents/pr-duty-rules-merge-method.md](spec/F7-duty-agents/pr-duty-rules-merge-method.md)
- [spec/F7-duty-agents/pr-duty-each-review-runs-in-its-own.md](spec/F7-duty-agents/pr-duty-each-review-runs-in-its-own.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs.md](spec/F7-duty-agents/pr-duty-how-a-review-runs.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-projects-never-wait-for-each-other.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-projects-never-wait-for-each-other.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-the-claim-posts-a-claim-event-nick-comment-on-the-pr-which.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-the-claim-posts-a-claim-event-nick-comment-on-the-pr-which.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-review-lasts-until-its-agent.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-review-lasts-until-its-agent.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-review-whose-pr-is-closed.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-review-whose-pr-is-closed.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-review-runs-in-the-pr.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-review-runs-in-the-pr.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-the-agent-records-decisions-only-through.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-the-agent-records-decisions-only-through.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-its-card-shows-n-current-step-the-feed-gets-the.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-its-card-shows-n-current-step-the-feed-gets-the.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-message-from-the-pr-duty.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-message-from-the-pr-duty.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-reviewed-by-reading.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-reviewed-by-reading.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-and-lands-the-approved-it-asks-rosen-to-land-the-f39eb6c5.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-and-lands-the-approved-it-asks-rosen-to-land-the-f39eb6c5.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-lands-the-approved-when-rosen-wants-a-receipt-it-101b5029.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-lands-the-approved-when-rosen-wants-a-receipt-it-101b5029.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-ands-the-approved-the-commands-the-check-set-the-81d5073a.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-ands-the-approved-the-commands-the-check-set-the-81d5073a.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch--the-approved-the-folder-holds-exactly-the-merge-3173977f.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch--the-approved-the-folder-holds-exactly-the-merge-3173977f.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved-every-command-passed.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved-every-command-passed.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-he-approved-a-successful-land-is-checkpointed-as-2dae756e.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-he-approved-a-successful-land-is-checkpointed-as-2dae756e.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch--and-lands-the-approved-it-takes-a-change-at-its-ef1e5b5e.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch--and-lands-the-approved-it-takes-a-change-at-its-ef1e5b5e.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch--lands-the-approved-each-check-takes-one-turn-in-1f184113.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch--lands-the-approved-each-check-takes-one-turn-in-1f184113.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved-a-command-failed.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved-a-command-failed.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved-the-merge-conflicts.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved-the-merge-conflicts.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-d-lands-the-approved-the-checks-could-not-run-at-e6a8c2dc.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-d-lands-the-approved-the-checks-could-not-run-at-e6a8c2dc.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-the-approved-this-serial-checker-is-retained-for-6520576b.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-the-approved-this-serial-checker-is-retained-for-6520576b.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-ies-builds-checker-and-post-land-messages-remain-0d35a4d4.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-ies-builds-checker-and-post-land-messages-remain-0d35a4d4.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-y-verifies-builds-fast-builds-in-after-land-mode-b5d1a96c.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-y-verifies-builds-fast-builds-in-after-land-mode-b5d1a96c.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-full-tests.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-full-tests.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-every-command-passed.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-every-command-passed.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-a-command-failed.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-a-command-failed.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-build.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-build.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-test.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-test.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-es-builds-the-results-contradict-each-other-such-c87369f4.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-es-builds-the-results-contradict-each-other-such-c87369f4.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-city-verifies-builds-the-checks-could-not-run-at-2238b38e.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-city-verifies-builds-the-checks-could-not-run-at-2238b38e.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-city-verifies-builds-a-project-with-no-post-land-0aca7f21.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-ch-city-verifies-builds-a-project-with-no-post-land-0aca7f21.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-reviewed-with-the-local-checks.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-reviewed-with-the-local-checks.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-the-review-runs-in-the-project.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-the-review-runs-in-the-project.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-checks-in-a-plain-merge-tree.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-checks-in-a-plain-merge-tree.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-the-agent-works-only-through-pr.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-the-agent-works-only-through-pr.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-requested-changes-go-to-the-author.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-requested-changes-go-to-the-author.md)
- [spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-a-land-rosen-refuses-because-the.md](spec/F7-duty-agents/pr-duty-how-a-review-runs-a-rosen-change-a-land-rosen-refuses-because-the.md)
- [spec/F7-duty-agents/pr-duty-approval-record.md](spec/F7-duty-agents/pr-duty-approval-record.md)
- [spec/F7-duty-agents/pr-duty-approval-record-github-s-review-state-is-advisory.md](spec/F7-duty-agents/pr-duty-approval-record-github-s-review-state-is-advisory.md)
- [spec/F7-duty-agents/pr-duty-approval-record-pr-duty-merges-only-the-head.md](spec/F7-duty-agents/pr-duty-approval-record-pr-duty-merges-only-the-head.md)
- [spec/F7-duty-agents/pr-duty-approval-record-a-branch-update-pr-duty-made.md](spec/F7-duty-agents/pr-duty-approval-record-a-branch-update-pr-duty-made.md)
- [spec/F7-duty-agents/pr-duty-approval-record-a-changes-event-newer-than-the.md](spec/F7-duty-agents/pr-duty-approval-record-a-changes-event-newer-than-the.md)
- [spec/F7-duty-agents/pr-duty-approval-record-on-a-repository-whose-branch-protection.md](spec/F7-duty-agents/pr-duty-approval-record-on-a-repository-whose-branch-protection.md)
- [spec/F7-duty-agents/pr-duty-review-the-intent-not-each-line.md](spec/F7-duty-agents/pr-duty-review-the-intent-not-each-line.md)
- [spec/F7-duty-agents/pr-duty-severity-bar.md](spec/F7-duty-agents/pr-duty-severity-bar.md)
- [spec/F7-duty-agents/pr-duty-severity-bar-everything-else-is-filed-as-an.md](spec/F7-duty-agents/pr-duty-severity-bar-everything-else-is-filed-as-an.md)
- [spec/F7-duty-agents/pr-duty-severity-bar-every-blocking-finding-goes-in-the.md](spec/F7-duty-agents/pr-duty-severity-bar-every-blocking-finding-goes-in-the.md)
- [spec/F7-duty-agents/pr-duty-severity-bar-when-a-reviewer-is-unsure-whether.md](spec/F7-duty-agents/pr-duty-severity-bar-when-a-reviewer-is-unsure-whether.md)
- [spec/F7-duty-agents/pr-duty-severity-bar-why.md](spec/F7-duty-agents/pr-duty-severity-bar-why.md)
- [spec/F7-duty-agents/pr-duty-review-the-statements-not-the-proofs.md](spec/F7-duty-agents/pr-duty-review-the-statements-not-the-proofs.md)
- [spec/F7-duty-agents/pr-duty-review-the-statements-not-the-proofs-does-each-statement-say-what-the.md](spec/F7-duty-agents/pr-duty-review-the-statements-not-the-proofs-does-each-statement-say-what-the.md)
- [spec/F7-duty-agents/pr-duty-review-the-statements-not-the-proofs-do-its-hypotheses-exclude-real-inputs.md](spec/F7-duty-agents/pr-duty-review-the-statements-not-the-proofs-do-its-hypotheses-exclude-real-inputs.md)
- [spec/F7-duty-agents/pr-duty-review-the-statements-not-the-proofs-is-the-cost-model-honest-and.md](spec/F7-duty-agents/pr-duty-review-the-statements-not-the-proofs-is-the-cost-model-honest-and.md)
- [spec/F7-duty-agents/pr-duty-pr-duty-writes-tests-too.md](spec/F7-duty-agents/pr-duty-pr-duty-writes-tests-too.md)
- [spec/F7-duty-agents/pr-duty-tiered-review.md](spec/F7-duty-agents/pr-duty-tiered-review.md)
- [spec/F7-duty-agents/pr-duty-tiered-review-trivial.md](spec/F7-duty-agents/pr-duty-tiered-review-trivial.md)
- [spec/F7-duty-agents/pr-duty-tiered-review-normal.md](spec/F7-duty-agents/pr-duty-tiered-review-normal.md)
- [spec/F7-duty-agents/pr-duty-tiered-review-sensitive.md](spec/F7-duty-agents/pr-duty-tiered-review-sensitive.md)
- [spec/F7-duty-agents/pr-duty-tiered-review-reading-the-diff.md](spec/F7-duty-agents/pr-duty-tiered-review-reading-the-diff.md)
- [spec/F7-duty-agents/pr-duty-tiered-review-sensitive-paths.md](spec/F7-duty-agents/pr-duty-tiered-review-sensitive-paths.md)
- [spec/F7-duty-agents/pr-duty-the-release-gate.md](spec/F7-duty-agents/pr-duty-the-release-gate.md)
- [spec/F7-duty-agents/pr-duty-the-release-gate-the-post-land-full-tests-green.md](spec/F7-duty-agents/pr-duty-the-release-gate-the-post-land-full-tests-green.md)
- [spec/F7-duty-agents/pr-duty-the-release-gate-cargo-run-p-stress-all-passing.md](spec/F7-duty-agents/pr-duty-the-release-gate-cargo-run-p-stress-all-passing.md)
- [spec/F7-duty-agents/pr-duty-the-release-gate-lake-build-in-proofs-passing-with.md](spec/F7-duty-agents/pr-duty-the-release-gate-lake-build-in-proofs-passing-with.md)
- [spec/F7-duty-agents/pr-duty-the-release-gate-no-open-issue-labelled-release-blocker.md](spec/F7-duty-agents/pr-duty-the-release-gate-no-open-issue-labelled-release-blocker.md)
- [spec/F7-duty-agents/pr-duty-landing-ticks-the-plan.md](spec/F7-duty-agents/pr-duty-landing-ticks-the-plan.md)
- [spec/F7-duty-agents/main-watcher.md](spec/F7-duty-agents/main-watcher.md)
- [spec/F7-duty-agents/main-watcher-after-each-merge-it-runs-every.md](spec/F7-duty-agents/main-watcher-after-each-merge-it-runs-every.md)
- [spec/F7-duty-agents/main-watcher-when-master-breaks-it-bisects-the.md](spec/F7-duty-agents/main-watcher-when-master-breaks-it-bisects-the.md)
- [spec/F7-duty-agents/main-watcher-targeted-delivery-tells-only-the-agents.md](spec/F7-duty-agents/main-watcher-targeted-delivery-tells-only-the-agents.md)
- [spec/F7-duty-agents/main-watcher-it-tracks-flaky-tests-and-files.md](spec/F7-duty-agents/main-watcher-it-tracks-flaky-tests-and-files.md)
- [spec/F7-duty-agents/main-watcher-never-push-to-master-every-fix.md](spec/F7-duty-agents/main-watcher-never-push-to-master-every-fix.md)
- [spec/F7-duty-agents/main-watcher-how-a-watch-runs.md](spec/F7-duty-agents/main-watcher-how-a-watch-runs.md)
- [spec/F7-duty-agents/main-watcher-how-a-watch-runs-the-runner-reads-the-base-branch.md](spec/F7-duty-agents/main-watcher-how-a-watch-runs-the-runner-reads-the-base-branch.md)
- [spec/F7-duty-agents/main-watcher-how-a-watch-runs-a-watch-is-one-agent-turn.md](spec/F7-duty-agents/main-watcher-how-a-watch-runs-a-watch-is-one-agent-turn.md)
- [spec/F7-duty-agents/main-watcher-how-a-watch-runs-the-watcher-records-verdicts-only-through.md](spec/F7-duty-agents/main-watcher-how-a-watch-runs-the-watcher-records-verdicts-only-through.md)
- [spec/F7-duty-agents/main-watcher-how-a-watch-runs-flaky-tests-are-filed-through-the.md](spec/F7-duty-agents/main-watcher-how-a-watch-runs-flaky-tests-are-filed-through-the.md)
- [spec/F7-duty-agents/main-watcher-how-a-watch-runs-a-message-from-the-main-watcher.md](spec/F7-duty-agents/main-watcher-how-a-watch-runs-a-message-from-the-main-watcher.md)
- [spec/F7-duty-agents/review-swarm-on-after-land-rosen.md](spec/F7-duty-agents/review-swarm-on-after-land-rosen.md)
- [spec/F7-duty-agents/review-swarm-on-after-land-rosen-review-swarm.md](spec/F7-duty-agents/review-swarm-on-after-land-rosen-review-swarm.md)
- [spec/F7-duty-agents/review-swarm-on-after-land-rosen-a-worker-claims-the-oldest-unreviewed.md](spec/F7-duty-agents/review-swarm-on-after-land-rosen-a-worker-claims-the-oldest-unreviewed.md)
- [spec/F7-duty-agents/review-swarm-on-after-land-rosen-findings-use-scout-issues-and-scout.md](spec/F7-duty-agents/review-swarm-on-after-land-rosen-findings-use-scout-issues-and-scout.md)
- [spec/F7-duty-agents/review-swarm-on-after-land-rosen-for-each-affected-f17-proof-list.md](spec/F7-duty-agents/review-swarm-on-after-land-rosen-for-each-affected-f17-proof-list.md)
- [spec/F7-duty-agents/review-swarm-on-after-land-rosen-completion-is-durable.md](spec/F7-duty-agents/review-swarm-on-after-land-rosen-completion-is-durable.md)
- [spec/F7-duty-agents/review-swarm-on-after-land-rosen-coverage-not-gates.md](spec/F7-duty-agents/review-swarm-on-after-land-rosen-coverage-not-gates.md)
- [spec/F7-duty-agents/review-swarm-on-after-land-rosen-this-runner-is-independent-of-the.md](spec/F7-duty-agents/review-swarm-on-after-land-rosen-this-runner-is-independent-of-the.md)
- [spec/F7-duty-agents/scouts.md](spec/F7-duty-agents/scouts.md)
- [spec/F7-duty-agents/scouts-assume-every-part-of-the-application.md](spec/F7-duty-agents/scouts-assume-every-part-of-the-application.md)
- [spec/F7-duty-agents/scouts-sweep-the-repo.md](spec/F7-duty-agents/scouts-sweep-the-repo.md)
- [spec/F7-duty-agents/scouts-focus-areas.md](spec/F7-duty-agents/scouts-focus-areas.md)
- [spec/F7-duty-agents/scouts-focus-areas-stress.md](spec/F7-duty-agents/scouts-focus-areas-stress.md)
- [spec/F7-duty-agents/scouts-focus-areas-proof-gaps.md](spec/F7-duty-agents/scouts-focus-areas-proof-gaps.md)
- [spec/F7-duty-agents/scouts-rules.md](spec/F7-duty-agents/scouts-rules.md)
- [spec/F7-duty-agents/scouts-rules-never-file-duplicates.md](spec/F7-duty-agents/scouts-rules-never-file-duplicates.md)
- [spec/F7-duty-agents/scouts-rules-no-issue-without-evidence.md](spec/F7-duty-agents/scouts-rules-no-issue-without-evidence.md)
- [spec/F7-duty-agents/scouts-rules-label-issues-for-dispatch.md](spec/F7-duty-agents/scouts-rules-label-issues-for-dispatch.md)
- [spec/F7-duty-agents/scouts-sweep.md](spec/F7-duty-agents/scouts-sweep.md)
- [spec/F7-duty-agents/scouts-sweep-each-scout-sweeps-once-when-it.md](spec/F7-duty-agents/scouts-sweep-each-scout-sweeps-once-when-it.md)
- [spec/F7-duty-agents/scouts-sweep-a-sweep-is-one-agent-turn.md](spec/F7-duty-agents/scouts-sweep-a-sweep-is-one-agent-turn.md)
- [spec/F7-duty-agents/scouts-sweep-scouts-file-only-through-velocity-s.md](spec/F7-duty-agents/scouts-sweep-scouts-file-only-through-velocity-s.md)
- [spec/F7-duty-agents/scouts-sweep-a-message-from-the-scouts-composer.md](spec/F7-duty-agents/scouts-sweep-a-message-from-the-scouts-composer.md)
- [spec/F7-duty-agents/scouts-sweep-scout-compare-runs-implementation-and-compiled.md](spec/F7-duty-agents/scouts-sweep-scout-compare-runs-implementation-and-compiled.md)
- [spec/F7-duty-agents/scouts-issues-they-file-show-their-name.md](spec/F7-duty-agents/scouts-issues-they-file-show-their-name.md)
- [spec/F7-duty-agents/accept-when.md](spec/F7-duty-agents/accept-when.md)
- [spec/F7-duty-agents/accept-when-pausing-finishes-in-flight-reviews-and.md](spec/F7-duty-agents/accept-when-pausing-finishes-in-flight-reviews-and.md)
- [spec/F7-duty-agents/accept-when-rail-pr-rows-show-the-review.md](spec/F7-duty-agents/accept-when-rail-pr-rows-show-the-review.md)
- [spec/F8-task-list.md](spec/F8-task-list.md)
- [spec/F9-issues-and-dispatch.md](spec/F9-issues-and-dispatch.md)
- [spec/F10-swarm.md](spec/F10-swarm.md)
- [spec/F11-agents-md-editor.md](spec/F11-agents-md-editor.md)
- [spec/F12-thread-context-menu-and-delete.md](spec/F12-thread-context-menu-and-delete.md)
- [spec/F13-settings.md](spec/F13-settings.md)
- [spec/F14-airc-claims-events-agent-messages.md](spec/F14-airc-claims-events-agent-messages.md)
- [spec/F15-providers-oauth.md](spec/F15-providers-oauth.md)
- [spec/F16-goal-swarm.md](spec/F16-goal-swarm.md)
- [spec/F17-proofs-and-adversarial-testing.md](spec/F17-proofs-and-adversarial-testing.md)
- [spec/F18-skills.md](spec/F18-skills.md)
- [spec/F19-code-graph.md](spec/F19-code-graph.md)
- [spec/5-keyboard-map.md](spec/5-keyboard-map.md)
- [spec/6-visual-language.md](spec/6-visual-language.md)
- [spec/7-implementation-order.md](spec/7-implementation-order.md)
- [spec/8-open-questions.md](spec/8-open-questions.md)
