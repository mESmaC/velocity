<!-- split-spec: {"order":1896,"kind":"markdown","indentation":0} -->
## 7. Implementation order

Each milestone ends with a working app.

**Bootstrap prerequisite** (AGENTS.md Proofs, F17)
- [x] Pinned Lake project, Lean 4, Mathlib, Charon and Aeneas, with documented setup and extraction commands

**M1 · Shell**
- [x] Tauri v2 app, frameless window, title bar, drag regions, window controls (F1)
- [x] Design tokens → theme module; bundled fonts
- [x] Three-column layout, sidebar groups, rail panels, pulse, palette, keyboard map (F1, §5)
- [x] Persisted state: projects, threads, tasks, settings

**M2 · Projects and spec**
- [x] Project store and switcher (F1)
- [x] New-project dialog: create a Rosen project from a folder with a Collapse, GitHub or no mirror (F2, #420)
- [x] Open project from the switcher: open a local Rosen project or import a Git checkout or GitHub repository into Rosen (F2, #420)
- [x] Spec interview: scan, ask, ready, lock; SPEC.md written and committed; draft → published tasks (F3)
- [x] Gate: no build work on unlocked projects (Rule 2)

**M3 · Threads and workspaces**
- [x] Agent runner: providers, models, effort, speed (Regular / Fast where the agent offers it), full permissions, queued steering (Rule 1, F4, F15)
- [x] Background thread titles from the title model (default GPT-6 Luna · Low · Fast), with the workspace name set from the first message (F4, F15)
- [x] ACP client (`agent-client-protocol`): spawn per thread, `session/new` in the workspace with AIRC as an MCP server, updates → transcript, auto-allow permissions, cancel, queued prompts, `fs`/`terminal` rooted at the workspace but not confined to it, full tool access with your config loaded and shared MCP servers (§3.2)
- [x] Optional context hand-off within a long Rosen work item, with on/off outcomes in Usage (§2.2, F15, #467)
- [x] Verify the agent's bypass mode at session startup and report a restricted mode before the first prompt (§3.2, #460)
- [x] Collapse's read-only MCP server for every agent: browser sign-in, tokens in the OS credential store, and an on/off switch in Settings → Agents → Shared MCP servers (§3.2, C9, #430)
- [x] Retry interrupted Rosen threads while their project server starts (§3.2, #476)
- [x] ACP agents for Claude Code (`claude-agent-acp`), Codex (`codex-acp`), Kimi Code (`kimi acp`) and Antigravity (`agy acp`), each using its own OAuth login through `authenticate`; Settings → Providers, model menu, limit meter (F15)
- [x] Custom ACP agents: Add an ACP agent (name, command, arguments), and an editable command on each provider card (F15)
- [x] Workspace manager: create / delete / prune, path scheme, setup step (copy files, run command, shared build cache) (F5)
- [x] Migrate saved Git projects and their work to Rosen in the background (F5, #419)
- [x] Thread view: transcript, tool steps, diffs, header with workspace path, composer, slash menu (F4)
- [x] Settings → Workspaces backed by Rosen, including guarded deletion, preparation changes and the live footer count (F5, F13, #423)

**M4 · Change follow-through**
- [x] GitHub observation for legacy PR threads (removed in #426)
- [x] Live Rosen status pane with server, base, landing, claims, warm pool, mirror and owning-thread navigation (F6, #421)
- [x] Rosen thread change track, header actions, land history and inline stored conflicts (F6, #422)
- [x] Open on Collapse for the project and each land, hidden without a Collapse mirror (F5, F6, #429)
- [x] Cut release for any landed commit while the mirror is Collapse: Rosen's last green commit, a workflow dispatch, and the tag created by the release workflow (F1, F6, #431)
- [x] Return refused Rosen lands to the author with repair instructions and show the refusal on the thread card (F6, #487)
- [x] Catch Rosen projects up from their mirror on start and from the project menu, never rebuilding the server (F5, #501)
- [x] Collapse as the default mirror: provision the repository for new projects and Mirror to Collapse for existing ones, push tokens in git's credential helper, and Collapse's forwarder state on the project (F2, F5, F6, #427)
- [x] PR follow-through: track, land history, auto-response to review comments, merge → workspace delete (F6; removed in #426)
- [x] Closed-without-merge state with the workspace kept (F5, F6; removed in #426)
- [x] Right-click menu and delete with 5 s undo (F12)
- [x] Shrink `velocity-github` to issues and repository listing, with no PR reads, polling or PR IPC (F6, F9, #426)

**M5 · Review**
- [x] Use after-land review for Rosen projects, with Scouts, the review swarm and Rosen R12 verification (F7, #424)
- [x] Scouts: sweeps, issue filing with repro, de-duplication (F7)
  - [x] Gate scheduled sweeps on base changes by default and keep idle review workers waiting for lands (#462)
  - [x] GitHub issue inventory (open and closed), executed failure evidence, duplicate assessment, dispatch labels and scout attribution; runnable backend filing path
- [x] Review view, start dialog, pause/resume, add agents (F7)

**M6 · Task List, issues, swarm**
- [x] Task List: states, add input, `T`, `/task`, palette, attribution, live agent state (F8)
- [x] Agent Task List tools, with autonomous edits and task pickup for continuous dispatch (F8, #439)
- [x] Issues panel and single dispatch (F9)
- [x] Show open GitHub blockers in the issues rail and skip blocked issues in continuous dispatch (F8, F9, #472)
- [x] Project Rosen work-item state into the live work pool (F8, F9, F16, #474)
- [x] Empty dispatch turns save their reason and hold the issue for independent review (F8, F9, #469)
- [x] Deduplicate recent discovery reports and hold overlapping issue dispatch for land verification (F9, F14, #471)
- [x] Swarm: dialog, launch, sidebar row, view, broadcast, + Items, rebase on merge, task done / issue close (F10)
- [x] Swarm and goal swarm follow the Rosen lifecycle with no PR duty capacity, PR state or git rebase path (F10, F16, #425)
- [x] Replace the Rosen Task List and Issues rails with Work (F8, F9, #475)

**M7 · AGENTS.md**
- [x] Outliner editor with markdown round-trip (F11)
- [x] Live lint, coverage checks with Add (F11)
- [x] Rewrite agent on Enter: tighten / expand / split / flag, with Undo (F11)
- [x] Default AGENTS.md template with the PR, Task List and AIRC rules (F11)

**M8 · AIRC**
- [x] Channel per project: log files, nicks, join/leave, MCP tools for agents (F14)
- [x] Claims from each agent's first step; overlap detection and `velocity` lines (F14)
- [x] Dependency wait → rebase → continue in the agent runner (F14)
- [x] Mention delivery to threads at step boundaries; your comments (F14)
- [x] Event log posted by Velocity from claims, GitHub and the Task List; agent messages kept separate (F14)
- [x] Modal: Events (default), Claims, Messages, Discoveries, Past work, nick list, unread badge, `A` (F14)

**M9 · Goal swarm**
- [x] Atomic claim lock: `airc_claim` / `airc_release`, refusal lines, held claims on failure (F14, F16)
- [x] AIRC MCP server injected into every provider's agent runtime (F14, F16)
- [x] Work pool: open Task List items, open issues, unbuilt SPEC.md sections (F16)
- [x] Dialog: prompt, count, model mix with "the rest", effort and speed per row, limits, pool size, keep going (F16)
- [x] Launch, choosing → claimed → normal lifecycle, leave when nothing is left, keep going (F16)
- [x] Fresh ACP sessions per work item with a bounded handoff and transcript boundaries (F8, F16, #456)
- [x] Native goal-swarm selection and claim retries, with a five-piece first-turn shortlist for custom goals (F16, #463)
- [x] Goal swarm view: claims board, one card per agent, activity; sidebar row (F16)

**M10 · Throughput** (§3.3)
- [x] Agent activity, stall detection with nudge then restart, per-agent caps, claim release after the grace period (§2.2)
- [x] Parking: end the session on `airc_wait`, resume on the subscribed `merged` event; optional provider concurrency cap (§2.2, F15)
- [x] Targeted AIRC delivery, message envelope (`to`, `intent`), heads-down and quarantine modes (F14)
- [x] Informational AIRC notices ride the next prompt, overlap files and idle deliveries batch together, and PR follow-through delivers review requests once (F6, F14, #464)
- [x] Targeted rebases, optimistic landing, conflicts sent back to the owning agent; rebuild the merge result whenever `master` moved; Shared files exempt from overlaps (F6, F14)
- [x] Warm workspace pool from one base clone, prebuilt at the tip of `master` with a shared compiler cache; workspace guards (F5)
- [x] Queue long agent commands in Rosen and resume the ACP thread with the result (#454, F5, F7)
- [x] Reasoning effort by role, with Medium defaults for trivial reading reviews and duty sweeps (F15, #468)
- [x] Batch integration builds on the base and queue single-crate agent checks (#480, F5, F7)
- [x] Apply the Rust command policy in agent shells and start agent checks at below-normal priority (#483, F5)

**M11 · Proofs** (F17)
- [x] Lean toolchain and Mathlib cache in the warm pool (F5)
- [x] Proof checks in the workspace: `lake build`, no `sorry`/`admit`, axiom audit, statement lock and review reset
- [x] Proof card on the PR, "Proved" on the PR track, `proved`/`statement`/`stress` events
- [x] Scouts: assume-it-fails sweeps, evidence required, Stress and Proof gaps focus areas, stress runs against Lean reference models
- [x] Levels per language: reference by default (fixture table plus one test; compiled Lean model through FFI for stress), code on request (Aeneas for Rust), model
- [x] AGENTS.md: Implementation first, Proofs, and Tests and docs sections; Covers check (F11)

**M12 · Skills** (F18)
- [x] Skill library: clone and pin packs, update with a diff, Windows symlink and UTF-8 handling
- [x] Settings → Skills: machine-wide library, default bundles, context budget, requirements, optional per-project always-on skills
- [x] Link always-on skills at session start and invoked skills when the turn starts; provider folders and local excludes
- [ ] Committed mode for always-on skills
- [x] Built-in packs: UI UX Pro Max (design system and AGENTS.md rule), claude-skills (bundles), Game dev skills (router and engine detection), REA (shared MCP server)
- [x] Project scan suggestions (F3), re-checked after lands
- [x] `$` skill menu in the composer; skill loads shown as tool steps (F4)

**M13 · Releases and updates** (F1)
- [x] In-app updates: signed releases from `v*` tags, the update check and Updates panel, Restart to update for setup, MSI and portable copies
- [x] Local builds of Velocity's own green commits: background build, Restart into `<commit>` and Restart into previous in the title bar (F1 Title bar, #512)

**M14 · Code graph** (F19)
- [x] Code graph index: tree-sitter symbols, files and subsystems with addresses, span hashes and edge provenance; the per-project parse cache, warmed after lands
- [x] `velocity-graph` MCP server in every session: `graph_find`, `graph_read`, `graph_impact` and `graph_run` over each agent's workspace view, with the span check (§3.2)
- [x] Per-project switch, and the graph on/off comparison of tokens and time per turn in the usage modal (F15 Usage modal)
- [x] Token usage by thread, role, project and input source, with day and week totals (F15 Usage modal, #461)
- [x] AIRC and Rosen overlaps by symbol, swarm split by subsystem, and the review swarm's graph impact set
- [x] Bound Velocity MCP answers and generated prompts, with issue search and paginated channel and Task List reads (#465)
- [x] Share the project graph, bound Velocity process memory, report memory in Usage and pause dispatch under memory pressure (#481)
- [x] Standalone `velocity-graph` indexer binary for Collapse: index a git commit from its objects and answer the four graph tools as versioned JSON, with a Linux x86-64 release build (C11, #428)

**M15 · Agent browser** (F20)
- [x] Private offscreen agent browser with ACP tools, live thread view, owner takeover, memory accounting and thread cleanup (#488)

---
