<!-- split-spec: {"order":1896,"kind":"markdown","indentation":0} -->
## 7. Implementation order

Each milestone ends with a working app.
PR duty checks an item off when it lands that item's PR (F7).

**Bootstrap prerequisite** (AGENTS.md Proofs, F17)
- [x] Pinned Lake project, Lean 4, Mathlib, Charon and Aeneas, with documented setup and extraction commands

**M1 · Shell**
- [x] Tauri v2 app, frameless window, title bar, drag regions, window controls (F1)
- [x] Design tokens → theme module; bundled fonts
- [x] Three-column layout, sidebar groups, rail panels, pulse, palette, keyboard map (F1, §5)
- [x] Persisted state: projects, threads, tasks, settings

**M2 · Projects and spec**
- [x] Project store and switcher (F1)
- [x] New-project dialog: local (git init) and GitHub (new/existing, visibility, clone) (F2)
- [x] Open project from the switcher: a local folder or one of your GitHub repositories; an existing SPEC.md locks the spec (F2)
- [x] Spec interview: scan, ask, ready, lock; SPEC.md written and committed; draft → published tasks (F3)
- [x] Gate: no build work on unlocked projects (Rule 2)

**M3 · Threads and workspaces**
- [x] Agent runner: providers, models, effort, speed (Regular / Fast where the agent offers it), full permissions, queued steering (Rule 1, F4, F15)
- [x] Background thread titles from the title model (default GPT-6 Luna · Low · Fast), with the workspace name set from the first message (F4, F15)
- [x] ACP client (`agent-client-protocol`): spawn per thread, `session/new` in the workspace with AIRC as an MCP server, updates → transcript, auto-allow permissions, cancel, queued prompts, `fs`/`terminal` rooted at the workspace but not confined to it, full tool access with your config loaded and shared MCP servers (§3.2)
- [x] ACP agents for Claude Code (`claude-agent-acp`), Codex (`codex-acp`), Kimi Code (`kimi acp`) and Antigravity (`agy acp`), each using its own OAuth login through `authenticate`; Settings → Providers, model menu, limit meter (F15)
- [x] Custom ACP agents: Add an ACP agent (name, command, arguments), and an editable command on each provider card (F15)
- [x] Workspace manager: create / delete / prune, path scheme, setup step (copy files, run command, shared build cache) (F5)
- [x] Thread view: transcript, tool steps, diffs, header with workspace path, composer, slash menu (F4)
- [x] Settings → Workspaces, including the live list and the footer count (F5, F13)

**M4 · PR loop**
- [x] GitHub integration: open PR, CI checks, reviews, comments, merge, close
  - [x] Live Open Changes rail: real project changes, CI, line counts, review state, reviewer claims and owning-thread navigation (F6/F7)
- [x] PR follow-through: track, land history, auto-response to review comments, merge → workspace delete (F6)
- [x] Closed-without-merge state with the workspace kept (F5, F6)
- [x] Right-click menu and delete with 5 s undo (F12)

**M5 · Duty agents**
- [x] PR duty: claiming, review against SPEC.md, request changes, approve, merge; never merge unapproved; approval record bound to the head SHA; severity bar (Rule 4, F7, Q28)
- [x] Scouts: sweeps, issue filing with repro, de-duplication (F7)
  - [x] GitHub issue inventory (open and closed), executed failure evidence, duplicate assessment, dispatch labels and scout attribution; runnable backend filing path
- [x] Duty view, start dialog, pause/resume, add agents (F7)

**M6 · Task List, issues, swarm**
- [x] Task List: states, add input, `T`, `/task`, palette, attribution, live agent state (F8)
- [x] Agent Task List tools, with autonomous edits and task pickup for continuous dispatch (F8, #439)
- [x] Issues panel and single dispatch (F9)
- [x] Swarm: dialog, launch, PR duty capacity, sidebar row, view, broadcast, + Items, rebase on merge, task done / issue close (F10)

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
- [x] Dialog: prompt, count, model mix with "the rest", effort and speed per row, limits, pool size, PR duty capacity, keep going (F16)
- [x] Launch, choosing → claimed → normal lifecycle, leave when nothing is left, keep going (F16)
- [x] Goal swarm view: claims board, one card per agent, activity; sidebar row (F16)

**M10 · Throughput** (§3.3)
- [x] Agent activity, stall detection with nudge then restart, per-agent caps, claim release after the grace period (§2.2)
- [x] Parking: end the session on `airc_wait`, resume on the subscribed `merged` event; optional provider concurrency cap (§2.2, F15)
- [x] Targeted AIRC delivery, message envelope (`to`, `intent`), heads-down and quarantine modes (F14)
- [x] Targeted rebases, optimistic landing, conflicts sent back to the owning agent; rebuild the merge result whenever `master` moved; Shared files exempt from overlaps (F6, F14)
- [x] Warm workspace pool from one base clone, prebuilt at the tip of `master` with a shared compiler cache; workspace guards (F5)
- [x] PR duty tiered review and intent review; Sensitive paths in AGENTS.md (F7)
- [x] Main watcher duty role: post-merge suite, bisect, fix or revert PR (F7)

**M11 · Proofs** (F17)
- [x] Lean toolchain and Mathlib cache in the warm pool (F5)
- [x] Proof checks in the workspace: `lake build`, no `sorry`/`admit`, axiom audit, statement lock and review reset
- [x] Proof card on the PR, "Proved" on the PR track, `proved`/`statement`/`stress` events
- [x] Merge gate in PR duty; statement review; tiers updated (F7)
- [x] Scouts: assume-it-fails sweeps, evidence required, Stress and Proof gaps focus areas, stress runs against Lean reference models
- [x] Levels per language: reference by default (fixture table plus one test; compiled Lean model through FFI for stress), code on request (Aeneas for Rust), model
- [x] AGENTS.md: Implementation first, Proofs, and Tests and docs sections; Covers check (F11)

**M12 · Skills** (F18)
- [x] Skill library: clone and pin packs, update with a diff, Windows symlink and UTF-8 handling
- [x] Settings → Skills: per-project switches, bundles, context budget, requirements, add any Agent Skills repo
- [ ] Link enabled skills into every worktree and the spec checkout per provider folder; `.git/info/exclude`; committed mode
- [x] Built-in packs: UI UX Pro Max (design system and AGENTS.md rule), claude-skills (bundles), Game dev skills (router and engine detection), REA (shared MCP server)
- [x] Project scan suggestions (F3), re-checked after lands
- [x] `$` skill menu in the composer; skill loads shown as tool steps (F4)

**M13 · Releases and updates** (F1)
- [x] In-app updates: signed releases from `v*` tags, the update check and Updates panel, Restart to update for setup, MSI and portable copies

**M14 · Code graph** (F19)
- [x] Code graph index: tree-sitter symbols, files and subsystems with addresses, span hashes and edge provenance; the per-project parse cache, warmed after lands
- [x] `velocity-graph` MCP server in every session: `graph_find`, `graph_read`, `graph_impact` and `graph_run` over each agent's workspace view, with the span check (§3.2)
- [x] Per-project switch, and the graph on/off comparison of tokens and time per turn in the usage modal (F15 Usage modal)
- [x] AIRC and Rosen overlaps by symbol, swarm split by subsystem, and the review swarm's graph impact set

---
