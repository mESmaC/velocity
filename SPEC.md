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
4. **Never merge without approval.** Only a PR duty agent that approved a PR merges it.
5. **One worktree per unit of work.** Every thread and every agent works in its own git worktree. The worktree is deleted as soon as that work's PR merges.
6. **AGENTS.md is direct and explicit.** Hedged or vague wording counts as a defect.
7. **Agents coordinate in AIRC through claims, not conversation.** Every agent claims its work and files, records a share or a wait when it needs a file another agent claimed, and waits for a PR it depends on to merge. Agents can message each other, but only when a claim, overlap or dependency needs words. A dependency wait is not an approval gate (Rule 1).
8. **Implementation first.** Agents build working functionality before anything else. Features on the project's proof list are proven in Lean after their code, in the same PR (F17); everything else ships with no Lean. Tests and docs are low priority: a building agent adds a test only for a critical contract, and scouts and PR duty write the adversarial tests. Only the agents whose job is to break things write tests and stress scripts: scouts and PR duty. Lean is the proving ground, not the test suite.

---

## 2. Concepts

| Term | Meaning |
|---|---|
| **Project** | A local folder or GitHub repo. It owns threads, a Task List, PRs, issues, AGENTS.md and SPEC.md. Velocity holds several projects and you switch between them. |
| **Thread** | One agent working on one piece of work, in its own worktree. |
| **Spec interview** | The first thread of a new project. It interviews you and produces SPEC.md plus the first Task List. |
| **Task List** | The project-level list of work, shown in the top-right panel. It is never a per-thread plan. |
| **Duty agent** | A long-running agent with a standing role: **PR duty** or **Scout**. Names are *(proposed)*. |
| **Swarm** | One agent per open item (tasks, issues, or both), all started at once. |
| **Goal swarm** | N agents given one identical prompt (e.g. "Read AGENTS.md and start dev.") with a model mix you set. Nobody assigns work: each agent claims its own piece in AIRC. |
| **Worktree** | A `git worktree` checkout made for one thread or one agent's job. |
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
| `working` | Running | Agent is working in its worktree. |
| `review` | PR open · watching | PR is open; the agent watches it. Stages: `ci`, `review`, `fixing`, `approved`. |
| `error` | Failed | Agent stopped on an error. Its worktree is kept. |
| `closed` | Closed · not merged | PR closed without merging. Its worktree is kept. |
| `done` | Done | Spec locked (spec threads only). |
| `merged` | Merged | PR merged and worktree deleted. The thread is finished. |

The sidebar shows groups in this order: draft, On duty, Swarms, Spec interview, Running, PR open · watching, Failed, Closed · not merged, Done, Merged. Empty groups are hidden, except "On duty", which shows a "start one" button when empty.

### 2.2 Agent activity

A thread's state (§2.1) says where it is in its lifecycle. Its **activity** says what its agent is doing right now. The two are tracked separately, as Google Scion does. Velocity sets activity from the ACP stream (§3.2) and from AIRC (F14).

| Activity | Meaning | Set by |
|---|---|---|
| `thinking` | The agent is producing output. | ACP message chunks |
| `executing` | A tool call is running. | ACP `tool_call` |
| `parked` | The agent is waiting on another agent's PR (`airc_wait`). Its ACP session is ended to free a provider slot, and its worktree and claims are kept. | `airc_wait` |
| `stalled` | No ACP update for 5 minutes *(proposed default)* while not parked. | Velocity |
| `at limit` | The agent hit a per-agent cap or its provider's subscription limit. This stays set until the agent resumes. | Velocity, provider |
| `idle` | The agent's turn ended and nothing is queued. | ACP turn end |

**Parked agents wake on events, not by polling.** A wait subscribes the agent to the `merged` event of the PR it waits on. When that event fires, Velocity resumes the session (`session/load`, or a new session with the transcript if the agent can't load one), rebases, and posts `unblocked`. If the PR is closed, fails or is deleted, the agent wakes and the wait ends.

**Stall handling:**
1. Velocity sends one prompt: "No output for 5 minutes. Continue, or say what is blocking you."
2. If there is still no update after another 5 minutes, Velocity cancels the turn and restarts the session in the same worktree from its transcript. The agent keeps its claims.
3. Each step posts a `stalled` event.

**Limits and claim release:**
- **Per-agent caps:** max turns and max duration, set in Settings → Agents (F13). Off by default.
- **Hitting a limit:** when an agent hits a cap or its provider's limit, its activity becomes `at limit`.
- **Release after a grace period:** if an agent stays `at limit`, or its thread has failed (§2.1 `error`), for 10 minutes *(proposed default)*, Velocity releases its claims. It posts a `released` event and the piece goes back to the pool (F16). The worktree is still kept (F5).
- **Closed PRs:** a closed PR keeps its claims until you delete the thread. Closing is a decision someone made, so it isn't treated as a failure.

**Sidebar and views:** `stalled` and `at limit` show in amber in the thread's sidebar meta and on its swarm or goal swarm card. `parked` shows "waiting for <nick> · #n".

---

## 3. Architecture

- **Shell:** Tauri v2, frameless window (`decorations: false`) with a custom title bar. Drag regions use `data-tauri-drag-region`; the window controls are custom.
- **Frontend:** runs in the webview: Svelte 5 + TypeScript on Vite (no SvelteKit), built with pnpm. Command and event types are generated from Rust with `tauri-specta`. All visual values come from the reference UI: its `:root` is copied exactly, and missing values are copied from its winning CSS declarations into supplemental reference tokens (§6).
- **Backend (Rust)** owns:
  - agent processes
  - git and worktrees
  - GitHub access
  - the file system
  - persisted state

  The frontend never runs a shell command. It calls Tauri commands and subscribes to events.
- **Agent providers:** Anthropic (Claude Code), Codex, Antigravity and Kimi Code. Every provider signs in with OAuth on your own subscription, the way T3 Code does. Velocity talks to each provider's own agent over ACP (§3.2) and never asks for or stores an API key (F15). Each agent records its provider, model, effort (Low / Medium / High / Max) and speed (Regular / Fast, F15).
- **GitHub:** issues, PRs, reviews, CI checks and merges.

### 3.1 IPC *(proposed names)*

| Kind | Name | Purpose |
|---|---|---|
| command | `get_window_state`, `minimize_window`, `toggle_maximize_window`, `close_window` | native main-window state and custom title-bar controls |
| command | `get_layout_state`, `dispatch_layout { action }` | backend-owned main-window projection, search, palette selection, Alt hints, contextual keyboard dispatch and navigation requests. Build intents are gated from the persisted active project's spec lock before publication; snapshots carry launch eligibility, rejection notices and draft-task hints. |
| command | `list_projects`, `create_project`, `switch_project` | projects |
| command | `get_new_project_defaults`, `pick_project_folder`, `create_project { request }` | New project dialog (F2): default folders, interview effort, speed and model from Settings; the native folder picker; Create, which refuses before anything runs or sets the project up, registers it, switches to it and records its spec interview |
| command | `list_threads`, `start_thread`, `send_message`, `delete_thread` | threads. `start_thread` and `send_message` are the composer's `StartPrompt` and `SteerThread` intents from `dispatch_layout`: the backend checks the launch gate afresh, records a `working` thread, creates its worktree on `vel/<slug>` (the message slug until thread titles land), starts the provider's ACP agent there and streams its transcript into `layout://state`. Messages to a busy agent are queued (Rule 1). |
| command | `list_pull_requests { project_identifier }`, `get_pull_request { project_identifier, number }`, `dispatch_pull_request { project_identifier, operation }` | GitHub through signed-in `gh`: paginated PR snapshots and typed create, review, comment, reply, resolve, review-request, merge and close operations. Create accepts `base_branch` (the project's configured branch), defaulting to GitHub's default branch. Approve and request changes follow F7's approval record (Q28): each needs a reason, records an `approved` / `changes` event (PR number, reviewer nick, head SHA) in the project's AIRC channel, and mirrors it on GitHub as a review starting "Approved at `<sha7>`" (a comment review when the login authored the PR, which GitHub refuses to approve). Window commands record as `you`. Merge requires that record: the latest approval with no newer `changes`, at exactly the head being merged, which is sent to GitHub as the expected head. GitHub's review state is not consulted. A refused merge posts a `failed` event; a merged same-repository topic branch is deleted. CI is advisory (F7). F6 observes same-repository PRs by the thread's immutable branch, including a PR closed or merged between observations. An open draft claim leaves its thread working until the PR is marked ready; a closed draft still reports closure. The native watcher checkpoints its history and pending agent notifications in `state.json`, queues changed CI/review/conflict information at agent step boundaries, and publishes the thread lifecycle. Closing preserves the checkout and branches; confirmed merges stop the agent and remove only its registered worktree through the existing F5 lifecycle plan, retrying cleanup errors. The live rail uses the independent `pr://panel` projection below. |
| command | `get_pull_request_panel`, `open_project_pull_request { project_identifier, number }` | Backend-owned active-project PR rail. Native refresh reads all open PRs, the current head's aggregate CI, line counts, review decision and the first explicit claim comment: `Reviewing · <nick>` names a PR duty agent, a bare `Reviewing` its author. A row opens its owning thread (recorded PR identity, then an unassigned matching branch from the same repository), otherwise GitHub in the browser. Clicks validate project identity. |
| command | `get_thread_menu { thread_identifier }`, `run_thread_menu_action { project_identifier, thread_identifier, action }`, `delete_thread { project_identifier, thread_identifier }`, `undo_delete_thread { deletion }` | Sidebar right-click menu and Delete with Undo (F12). The backend builds the menu from saved state, the agent session and the worktree's measured size. Actions: open the worktree in Explorer, copy its path (with a toast), open the PR in the browser. `delete_thread` hides the thread at once and publishes a toast carrying the deletion's Undo token on `layout://state`. Nothing is touched for 5 s. Then the backend stops the agent, removes the worktree (the worktree lifecycle's `UserDeletedThread`), closes any open PR, returns the thread's tasks to todo and drops its record. A worktree that cannot be removed keeps the thread and shows why. |
| command | `get_launch_eligibility { request }` | UI eligibility for the active project, read from the backend state. No project/lock flag is accepted from the frontend. Launch handlers check the guard afresh before worktree or agent effects (Rule 2, F3). |
| command | `get_duty_dialog_defaults { project_id }`, `start_duty { request }`, `pause_duty { request }`, `add_duty_agents { request }` | Duty view, start dialog defaults, start/pause/resume and add-agents lifecycle, persisted on the project in `state.json` and broadcast over `duty://event` (F7) |
| command | `get_issue_panel`, `open_project_issue { project_identifier, number }`, `dispatch_issue { project_identifier, number, accept_recommendation }` | Active-project issue rail (F9). Native refresh reads open issues, labels, scout attribution and spec-question recommendations. Opening an assigned issue selects its thread; otherwise it opens GitHub. Dispatch re-reads the issue, checks the saved spec lock and atomically records issue ownership before starting the ordinary worktree/ACP lifecycle. Accept scopes that thread to the recommended one-line proposed SPEC.md answer. |
| command | `launch_swarm`, `add_to_swarm` | swarm |
| command | `launch_goal_swarm { prompt, mix: [{ model, effort, speed, count }], keep_going }`, `add_goal_agents` | goal swarm |
| command | `add_task`, `list_tasks` | Task List |
| command | `read_agents_md`, `write_agents_md`, `tighten_rule { project_identifier, identifier }` | AGENTS.md. F11 rewrites use the normal unrestricted Claude ACP client in the project checkout, with the Haiku model selected through the Claude SDK launch environment and whole-file context. Late results are discarded after edits, Undo or save; invalid responses and failures retain the original rule with an explanation. |
| command | `read_agents_md { project_identifier }`, `edit_agents_md { project_identifier, action }`, `write_agents_md { project_identifier }` | F11 outliner: read the checkout's real AGENTS.md, apply section/rule and structural keyboard edits in Rust, and save/commit only that file. Snapshots include the exact Markdown, rule count, dirty state and focus target. Unsupported Markdown is preserved verbatim. Saving rejects external changes and keeps the draft; a failed commit can be retried. |
| command | `get_settings`, `set_settings`, `list_worktrees`, `prune_worktrees` | settings |
| command | `set_worktree_settings { edit, project_identifier }`, `preview_worktree_folder { folder }`, `browse_worktree_folder` | persist a worktree preference edit; preview the path for a folder still being typed, without saving it; select a location through the native folder picker |
| command | `list_providers`, `provider_sign_in`, `provider_sign_out`, `provider_models` | providers (OAuth) |
| command | `airc_send { project_identifier, text }` | Your F14 comment in a registered project channel. An exact `nick:` prefix targets that member; an unknown nick is rejected. Other comments broadcast to active thread members. Delivery uses ordinary queued steering and never cancels a running turn. |
| command | `airc_events { project_identifier, after }`, `airc_messages { project_identifier, after }`, `airc_send`, `airc_claims`, `airc_claim`, `airc_release`, `airc_share`, `airc_wait`, `airc_ledger { project_identifier, ledger, after }` | AIRC (agents get the same as MCP tools). `airc_events` returns only F14 event log entries (`EVENT_TAGS`, including `discovery` notices); `airc_messages` returns only agent messages and user comments; `airc_ledger` reads `discovery` or `past_work`. `airc_claim` is atomic: it returns `granted`, or `refused { holder, since }` |
| command | `dispatch_layout` with `Navigation(Answer(n))`, `Navigation(Lock)`, `Navigation(SpecFile)`, `Navigation(Grill)`, `SteerThread` on a spec thread, and `ToggleProofListEntry(index)` | spec interview (F3): answer `1`–`4` or in your own words, Lock spec & publish tasks, open SPEC.md, Keep grilling, and switch a proposed proof-list entry. The backend carries these out; the interview's phase, question, decisions and proof list arrive on `active_thread.interview` in `layout://state`. |
| event | `thread://status`, `thread://step` | thread state and tool steps: `thread://status` carries a thread's state and activity (§2.1, §2.2) whenever either changes; `thread://step` carries the thread's newest transcript block after each agent update |
| event | `window://state` | native maximize/restore state, including changes made by the operating system |
| event | `layout://state`, `layout://intent` | ordered main-window snapshots and typed navigation requests for feature owners; no shell commands cross IPC |
| event | `project://selected` | persisted project selection and refreshed switcher rows, paired with the selected project's `layout://state` snapshot |
| event | `pr://opened`, `pr://ci`, `pr://review`, `pr://merged`, `pr://closed` | F6 lifecycle snapshots: project/thread identifiers, PR number, stage and worktree-deleted status. `pr://merged` follows successful worktree cleanup; the layout carries the history and review cards. |
| event | `pr://panel` | Ordered active-project PR panel snapshots, refreshed natively after project selection and at 30-second intervals. Loading and failed refreshes are explicit; failed refreshes retain the last rows with the error. The UI never polls GitHub. |
| event | `duty://event`, `swarm://event` | activity feeds |
| event | `issue://panel` | Ordered active-project issue snapshots, refreshed natively on project selection and every 30 seconds, with owning-thread state updated from saved changes. Failed refreshes retain the previous rows with the error. |
| event | `worktree://created`, `worktree://deleted` | worktree lifecycle |
| event | `worktree://inventory` | cached live worktree rows, sizes, drive capacity and settings; metadata measurements run outside the UI thread |
| event | `airc://event`, `airc://message`, `airc://overlap` | AIRC events, agent messages and overlap detection |
| event | `provider://status`, `provider://limits` | sign-in state and subscription limits |


### 3.2 Agent protocol: ACP

Velocity is an **ACP (Agent Client Protocol) client**, as T3 Code, Zed and most other code editors are. Every agent runs as a subprocess that speaks ACP: JSON-RPC 2.0 over stdio.
- **One process per thread**, launched in that thread's worktree.
- **Client:** the Rust backend, using the `agent-client-protocol` crate.
- **Provider code:** everything provider-specific is just which command to launch. The four providers differ in nothing else.

| Provider | ACP agent |
|---|---|
| Anthropic | Claude Code through the `claude-code-acp` adapter |
| Codex | Codex through the `codex-acp` adapter |
| Kimi Code | Kimi CLI's own ACP mode (`kimi --acp`) |
| Antigravity | not confirmed (Q14) |

How Velocity's features map onto ACP:

| Velocity | ACP |
|---|---|
| Sign in (F15) | `initialize` returns the agent's `authMethods`. `authenticate` runs that agent's own OAuth login in your browser. Tokens stay in the agent's credential store. |
| Start a thread | Spawn the agent with your own config loaded (Tool access, below), and call `session/new` with `cwd` set to the thread's worktree and `mcpServers` set to AIRC plus your shared MCP servers. |
| AIRC tools for every agent (F14) | The AIRC MCP server is passed in `session/new`, so every provider gets the same tools with no per-provider setup. |
| Prompt, queued steering (Rule 1) | `session/prompt`. A steering message sent mid-turn is queued and sent as the next prompt when the turn ends. |
| `Esc` interrupt | `session/cancel` |
| Transcript, tool steps, diffs (F4) | `session/update` notifications: message chunks, `tool_call` / `tool_call_update` (kind, status, file locations, diff content) and plan updates |
| Slash menu (F4) | `available_commands_update` |
| Unrestricted agents, no approval UI (Rule 1) | Velocity answers every `session/request_permission` at once with the agent's allow-always option. Where an agent offers a bypass mode, `session/set_mode` selects it when the session starts. Permission requests are never shown to you. |
| Model, effort and speed | The agent's session model and config options where it exposes them, otherwise launch arguments. Whether a model has a fast mode is read from the agent's config options, not hard-coded. |
| File edits and claims (F14) | Velocity advertises the `fs` and `terminal` client capabilities, with relative paths resolved against the thread's worktree. They are not confined to it (Tool access). Edits and tool-call file locations feed the diff view and the claim checker. If an agent edits a file it hasn't claimed, the file is added to its claim, an overlap check runs, and a `claim` event is posted. |
| Resume after a restart | `session/load`, where the agent supports it. Otherwise the thread restarts from its transcript. |
| Subscription limits (F15) | Not part of ACP. They are read per provider (Q14). |

**Tool access.** Every agent gets every tool, whatever its role: threads, swarm and goal swarm members, PR duty, scouts, the main watcher and spec interviews. Velocity adds tools and never removes any.
- **Built-in tools:** each provider's agent keeps its whole native toolset (shell, file read/write/edit, search, web search and fetch, subagents, todo/plan, and anything else it ships). Velocity never passes an allow-list or deny-list, and never starts an agent in a restricted or read-only mode.
- **Your own config:** each agent is launched so it loads your user and project config as it would in a terminal: its MCP servers, skills, slash commands, hooks and instruction files (`CLAUDE.md`, `AGENTS.md`). Velocity doesn't launch agents in an isolated or empty config.
- **Skills:** the project's enabled skill packs (F18) are linked into every agent's worktree, in the folder its provider reads, on top of your own skills.
- **Shared MCP servers** (Settings → Agents): servers listed here are passed to every agent of every provider in `session/new`, so one list reaches all four providers. When a name is also in an agent's own config, the agent's own entry wins.
- **AIRC:** always passed (F14).
- **Shell, network, toolchains:** the shell runs as you, with your `PATH`, toolchains (cargo, lake, node, python…), credentials and network. There is no sandbox or container.
- **GitHub:** agents use your signed-in `gh` and git credentials to push, open PRs, review, comment and merge (merging stays PR duty's job, Rule 4).
- **Worktree:** the working directory, not a jail. Agents can read anything on the machine, such as other repos, docs and caches. Their edits belong in their own worktree (AGENTS.md); edits elsewhere aren't blocked.
- **Permissions:** answered allow-always as above (Rule 1).

**Accept when:**
- [ ] Every agent's tool list, as reported in its first `tool_call`s and available commands, matches the same agent run in a terminal with the same config, plus AIRC and the shared MCP servers.
- [ ] A shared MCP server added in Settings → Agents is available to new agents of all four providers without per-provider setup.
- [ ] No role, including duty agents and spec interviews, runs with fewer tools than a thread.

### 3.3 Prior art: Scion

Two projects called Scion informed how agents collaborate in Velocity and how their work lands fast.
- **Anthropic's Scion** ([design post](https://www.anthropic.com/engineering/redesigning-source-control); an independent reimplementation is [CarterT27/scion](https://github.com/CarterT27/scion)) is source control rebuilt for agent-scale merging. It runs at over 30,000 merges a day.
- **Google's Scion** ([GoogleCloudPlatform/scion](https://github.com/googlecloudplatform/scion)) orchestrates teams of agents.

| Idea | From | Where in Velocity |
|---|---|---|
| Each agent receives only what concerns it; messages carry `to` and `intent`; "Not in `to` → not for you" | Google | F14 targeted delivery |
| Status notices always get through; agent messages respect per-agent modes (heads-down, quarantine) | Google | F14 |
| No global merge lock: optimistic landing; the agent that owns a change fixes its conflicts | Anthropic | F6 landing |
| Rebase only the agents a merge affects | Anthropic (claims are Velocity's own) | F6 landing |
| One base clone, cheap worktrees per agent, shared cache | both | F5 warm pool |
| Activity separate from lifecycle; stall detection; limits that release work | Google | §2.2 |
| Event-driven wake of waiting agents (notification subscriptions); parking to free slots | Google | §2.2 |
| Review intent, not lines; tiered review by risk | Anthropic | F7 |
| Post-merge testing, and agents that watch one area of the system | Anthropic | F7 Main watcher |

**Not adopted:**
- **A lead agent with silent workers** (Google's documented team pattern). Velocity's goal swarm has no leader and coordinates through locked claims instead (F16).
- **A global merge queue.** Anthropic dropped theirs at agent scale.
- **Containers per agent.** Worktrees give enough isolation for now.

---

## 4. Features

### F1 · Main window

The layout has three columns:
- **left:** sidebar, 236 px
- **middle:** the main view
- **right:** a rail, 236 px

**Title bar:**
- **Brand and project switcher:** a dropdown listing every project. Each entry shows whether it is local or on GitHub, or "speccing" if its spec isn't locked yet.
  - Switching restores that project's saved active thread, sidebar and rail context, clears transient search and input, and closes the dropdown. The last active project and each project's active thread survive restart. Unknown project IDs leave the current selection unchanged.
- **Pulse:** live counts, each shown only when it is non-zero:
  - on duty
  - in spec
  - running
  - PRs watched
  - failed
  - merged today
- A `Ctrl K` button that opens the palette.
- The window controls.

**Sidebar:**
- search (`/`) and a new-thread button
- the thread groups (§2.1)
- a footer with settings, usage, updates, a live worktree count ("8 on D:", click → Settings · Worktrees) and a subscription-limit meter: one bar per signed-in provider, filled to the share of its limit used, coloured by provider (click → Settings · Providers). There is no dollar spend: every provider runs on a subscription.

**Rail:** three panels: Task List, Open Pull Requests, Open Issues. Below them are footer links: AIRC (F14), with an unread badge that turns amber when an overlap is unread, and AGENTS.md.

**Home (empty) view:** a composer and quick chips:

| Chip | Key |
|---|---|
| Dispatch an issue | `I` |
| Resume last thread | `R` |
| Fan out ×3 | `F` |
| Put an agent on PR duty | `V` |
| Send scouts | `S` |
| Swarm the Task List | `W` |

**Command palette** (`Ctrl+K`): fuzzy search over commands, projects, threads and issues. Commands:
- new thread
- new project
- edit AGENTS.md
- add task
- settings
- dispatch issue
- fan out
- PR duty
- scouts
- swarm the Task List
- swarm the issues

**Accept when:**
- [ ] Window is frameless and can be dragged by the title bar. The custom window controls work.
- [ ] Pulse updates live from events and hides zero counts.
- [ ] Holding `Alt` shows numbers on threads, and `Alt+1`–`9` jumps to that thread.
- [ ] Every keyboard shortcut in §5 works.

### F2 · New project

`Ctrl+Shift+N` opens a dialog with:
- **Location:**
  - **Local folder:** a path and Browse… (native picker via `tauri-plugin-dialog`). One option: "create the folder and run `git init` if needed".
  - **GitHub:** New repo or Existing repo; `github.com/<owner>/<name>`; Private or Public; and a "clone to" path.
- **Model for the interview:** model cards plus effort and speed.
- **What are you building?** A free-text brief.
- **Create & start interview** (`Ctrl+Enter`).

**Accept when:**
- [ ] Create sets up the project (folder / `git init` / repo / clone), switches to it, and starts the spec interview thread straight away.
- [ ] Esc cancels; nothing is created.

### F3 · Spec interview

1. **Scan:** the agent reads the folder or repo and lists what it found as tool steps. It works in the main checkout and gets **no worktree**.
   - The scan also suggests skill packs for the project (F18): UI UX Pro Max when it finds a UI, and Game dev skills when it finds a game engine. Suggestions are switched on unless you turn them off.
   - When UI UX Pro Max is on and the project has a UI, the interview asks the design questions it needs (product type, audience, mood) and generates the design system before the spec locks.
2. **Ask:** one question at a time.
   - Each question has a short "why it matters", up to 4 options (one marked *recommended*, answered with keys `1`–`4`), and a free-text answer box.
   - Each answer is recorded as a decision (key: value) and can add draft tasks to the Task List. Draft tasks show with a dashed icon.
   - The agent keeps asking until nothing is ambiguous.
3. **Ready:** a spec card lists every decision.
   - **Proof list:** the card proposes which features get Lean proofs (F17), and you edit it before locking. Most projects list a handful; UI and plumbing never appear on it.
   - **Lock spec & publish tasks** (`L`): commits SPEC.md, turns the draft tasks into real tasks, and marks the thread done.
   - **SPEC.md** (`S`): opens the file.
   - **Keep grilling** (`G`): asks more questions.
4. **How the agent reports structure:** the interview runs on any provider, so the prompt carries the whole protocol. The agent ends each reply with a fenced `velocity-interview` JSON block: the scan summary (first reply), the draft tasks the last answer adds, and either the next question (`key`, `text`, `why`, up to 4 options, one recommended) or `ready` with the proposed proof list. Velocity hides the block from the transcript. A reply without one leaves the question open for a free-text answer. Lock writes the edited proof list into SPEC.md's `## Proof list` section before committing. After a restart, an interview resumes on your next message from its saved decisions (§3.2 Resume after a restart).

**Accept when:**
- [ ] No build thread, swarm or dispatch can start on a project whose spec is unlocked. Swarm marks draft tasks "lock the spec first".
- [ ] Locking commits SPEC.md and publishes the tasks.
- [ ] The Task List counter shows `draft · N` before lock and `done/total` after.

### F4 · Threads and composer

- **Sending from the home composer** starts a thread. It gets a worktree from `master`, a branch `vel/<slug>`, and starts at once.
- **Thread titles:** the title model (Settings → Providers → Thread titles, F15) names the thread from your first message, as a short imperative title.
  - Velocity asks for the title while it takes a worktree from the warm pool (F5). Both finish in under a second with the default model, so the branch and worktree are named from the title (`vel/<title-slug>`) before the agent starts.
  - Until the title arrives, the sidebar shows the first words of your message, pulsing, and the header shows "titling · <model>".
  - If the title model fails or takes longer than 2 seconds, the slug of your message is kept.
  - Threads are titled once, from the first message. Later messages don't retitle them.
  - Swarm, goal swarm and dispatched threads take their item's title and don't call the title model.
- **Thread header:**
  - title
  - branch
  - worktree path (click → open in Explorer)
  - +/− line counts and age
  - Diff (`D`), Editor (`E`), and `PR #n ↗` once a PR exists
- **Transcript:** user message, agent messages, tool steps (verb, argument, result, running indicator) and diffs.
- **Composer:**
  - `Enter` sends, `Shift+Enter` adds a new line, `Esc` leaves the box.
  - Model pill (provider, model, effort, and "· Fast" when Fast is on), attach, mic. The pill opens a menu grouped by provider: each signed-in provider with its models and limit used, each signed-out provider as a "Sign in with …" row, and effort and speed at the bottom. On a running thread, a new model or speed applies from the next message.
  - A message sent while the agent is busy shows "queued · sends when the current step finishes".
- **Slash menu:** `/task`, `/fanout 3`, `/review`, `/compact`, `/model`.
- **Skill menu:** typing `$` lists the project's enabled skills with a one-line description each, filtered as you type. Picking one inserts `$<skill>`, and the agent is told to load that skill for this message. The transcript shows each skill an agent loads as a tool step ("Skill · ui-ux-pro-max").
- **Fan out** (`F`): the same prompt goes to 3 models in 3 worktrees.

**Accept when:**
- [ ] Messages never interrupt a running agent; they are queued (Rule 1).
- [ ] A finished task opens its own PR. There is no "Ship PR" button or `/ship` command.

### F5 · Worktrees

**Lifecycle (decided):**

| When | What happens |
|---|---|
| A thread starts, or any agent starts a job (PR duty review, scout sweep, swarm member, dispatched issue) | Create a worktree from `master`. Its first transcript step is `Worktree · <path> · created from master`. |
| Its PR merges | Delete the worktree. Delete the local branch too if that setting is on (default on). Log both as steps with the space freed. |
| Its PR is closed without merging | **Keep** the worktree. No automatic expiry. |
| The thread fails | **Keep** the worktree. |
| The user deletes the thread | Delete the thread and its worktree together (F12). |
| Spec interview | No worktree. |

A worktree's path is fixed when it is created. Changing the location setting affects only worktrees created afterwards.

**Path:** `<drive><folder>\<project>\<branch-without-vel/>`, e.g. `D:\velocity\worktrees\velocity\command-palette`. Duty jobs use `review-pr-<n>` and `<agent>-sweep`.

**Settings → Worktrees:**
- **Location:**
  - a drive picker showing each drive's name, type, used bar and free space (slow drives get a warning)
  - a folder field with Browse…
  - a live path preview
  - a per-project override ("Use a different location for <project>")
  - a notice when the location changed while worktrees exist
- **Lifecycle:** the two always-on rules shown locked; "delete the local branch too"; a read-only explanation of the closed, failed and spec rules.
- **New worktree setup:**
  - untracked files to copy (default `.env, .env.local`)
  - a command to run after creation (default `pnpm -C ui install`)
  - "share the Rust build cache between a project's worktrees" (default on)

  The agent starts only after setup finishes.
- **Live worktrees:** one row per worktree: state dot, path, owner, size and state (working / PR #n / failed · kept / closed · kept / review / sweep). Just-deleted rows show briefly, crossed out. The header shows the count, total size and free space on the drive. **Prune orphans** runs `git worktree prune`. Clicking a row opens its thread.

**Warm pool (speed):**
- **One base clone.** Velocity keeps one base clone per project, and every worktree is a `git worktree add` from it, so only the first worktree pays for a clone.
- **Ready-made worktrees.** Velocity keeps a few worktrees ready at the tip of `master` with setup already done: files copied, setup command run, shared build cache linked. The count is 4 by default and set by "Keep N worktrees ready" on this page.
- **Taking one.** A new thread or agent takes a ready worktree, renames it to its path, and creates its branch there. If `master` has moved since the worktree was made, it fast-forwards first, which is cheap.
- **Refilling.** The pool refills in the background. Ready worktrees fast-forward after each merge to `master`, then re-run setup only if the dependency manifests changed, and build the project at the new tip, so an agent's first build compiles only what it changes.
- **Skills.** Pool setup links the project's enabled skills into the worktree (F18), so an agent never starts without them.
- **Lean.** Pool setup installs the project's pinned Lean toolchain (`lean-toolchain`, through elan) and runs `lake exe cache get`. Mathlib's built files live in the shared build cache, so a worktree never rebuilds them.
- **Swarm launches.** Launching a swarm or goal swarm tops the pool up to its agent count. Agents that find no ready worktree wait for one rather than racing each other on setup.
- **Build queue** *(proposed)*. Velocity runs agents' heavy builds (compile, test, lint, the PR duty landing check) through one machine-wide queue, one build at a time at full machine resources. Builds run side by side split the cores and memory and all finish late together; run one at a time, the first in line finishes in a fraction of the time and the average wait drops. Order: landing checks first, then checks before a PR is marked ready, then everything else, first come, first served within each. The footer shows the running build and the queue. Long-running dev servers never hold the queue. Until Velocity runs builds itself, `scripts/build-queue.ps1` is the stand-in: a named machine-wide lock, released by Windows when its holder exits.

**Guards** (from Google Scion's worktree work):
- Velocity is the only thing that creates worktrees, and agents never run `git worktree add` themselves (AGENTS.md).
- Deleting a worktree removes only that worktree's own entry. A repo-wide `git worktree prune` runs only from the **Prune orphans** button.
- Worktree folder names are unique per project, never generic names like `workspace`.

**Accept when:**
- [ ] With a ready worktree in the pool, an agent starts working within 5 seconds of being launched.
- [ ] Two agents never share a worktree.
- [ ] Every merge deletes its worktree within one event cycle.
- [ ] Closed and failed threads keep their worktree until the thread is deleted.
- [ ] Changing the location never moves or breaks existing worktrees.
- [ ] Sharing the build cache uses one compiler cache shared by every worktree (sccache as `RUSTC_WRAPPER` for Rust) and a separate target folder per worktree *(proposed mechanism)*. A shared `CARGO_TARGET_DIR` is not used: cargo locks it, so agents would build one at a time.
- [ ] Two agents' heavy builds never run at the same time, a landing check never waits behind a lower-priority build, and the footer shows the running build and the queue *(proposed)*.

### F6 · PR follow-through

When the task is done, the agent opens a PR. The PR description links the Task List item or says `Fixes #n`. The thread moves to **PR open · watching**.

Draft claims keep the coding transcript until the PR is ready. The watcher follows the thread's same-repository branch and excludes historical PRs from earlier uses of that branch. New review work is saved before delivery and remains pending until its ACP prompt completes; after a restart it resumes in the retained worktree. The PR author's own progress replies remain in history without prompting another response. Merge cleanup and explicit deletion (F12) share the thread's lifecycle lock and wait for confirmed shutdown of its agent and owned terminals before removing the checkout.

- **PR track** under the header: Task done → Proved → PR #n → CI → Review → Merged → Worktree deleted. A closed PR shows: … → Closed without merging → Worktree kept.
- **The transcript switches to the PR history:**
  - the coding work, collapsed to one line ("N tool calls · N files · +a −d · proved" with a Show work button)
  - PR opened, then the **proof card** (F17), CI results and reviewer claims
  - **review comments** as cards with file:line and open / ✓ resolved
  - the fix steps (edit, prove, push, reply) and "re-requested review"
  - approval, then a merged card
- **Agent behaviour:**
  - If the local checks or CI fail on its own change, fix and push.
  - Answer every review comment: either fix and push, or reply with the reason. Then resolve the thread and re-request review.
  - If the PR is closed without merging, report why in the thread.
- **Status line:** "watching PR #n · <stage> · answers new comments as they arrive", or "addressing N open review comment(s)".
- **Sidebar meta:** `#n` plus the stage label in the stage colour: checking / in review / addressing review / approved · merging.

**Landing and rebases** (from Anthropic's Scion):
- **No global merge queue or lock.** PR duty merges a PR as soon as it is approved and its local checks pass (F7), without waiting its turn behind other PRs.
- **Targeted rebases.** When a PR merges, Velocity compares the merged change's files with each open agent's claimed files and edited files (§3.2).
  - Only agents with an overlap rebase straight away; each gets a `rebased` event.
  - Every other agent rebases once, at landing time, instead of after every merge.
  - Generated files and registration lists (AGENTS.md "Shared files", F14) never count as an overlap. Generated files are regenerated, and registration lists take both sides' additions.
  - For edited files, an overlap means the changed regions touch or sit within a few lines of each other, not just that both changes are in the same file *(proposed)*.
- **PR duty updates branches.** PR duty agents are allowed, and encouraged, to update any open PR's branch with `master`. They don't wait for the authoring agent to rebase.
  - **When:** whenever a PR they are reviewing or about to merge is behind `master`, and again just before merging.
  - **How:** a merge of `master` into the PR branch (`gh pr update-branch`, or a merge commit pushed to the branch). PR duty never force-pushes and never rewrites the author's commits, so the author's history and open review threads stay intact. Each update posts an `updated` event naming the PR and the new head.
  - **Why:** waiting for each author to notice and rebase was the main source of landing delays. The author may be busy, rate-limited or paused, and every merge to `master` restarted the wait.
  - **The author** gets the `updated` event in its thread and pulls before it pushes again, so its next push builds on PR duty's update instead of being rejected.
- **Landing on a moved `master`.** PR duty updates the branch with `master` just before merging, then runs the checks on the merge result, not on the old PR head.
  - **`master` unchanged since the checks ran:** PR duty merges.
  - **No overlapping files:** if it applies cleanly and none of its files changed on `master` since the checks ran, PR duty rebuilds the merge result (incremental build and lint) and merges. It skips only the rest of the checks. A clean textual merge can still break the build: a field one PR adds to a struct breaks another PR's struct literal in a file the first PR never touched.
  - **Overlapping files, clean apply:** if files changed on `master` but the update applies cleanly, the checks run again first.
  - **Conflict PR duty can resolve:** when both sides are clear and the resolution keeps both changes as written (imports, lists, neighbouring hunks, lockfiles regenerated), PR duty resolves it in its review worktree, pushes the merge commit and re-runs the checks.
  - **Conflict that needs the author:** when resolving would mean choosing between two behaviours, the PR goes straight back to its own agent, with a `conflict` event naming the files. The agent resolves the conflict and pushes, then PR duty re-checks only the changed hunks and merges. A conflict is never routed through you.
- **Merge order** (`order` events, F7) is set only for PRs that overlap each other. Unrelated PRs never wait on each other.

**Accept when:**
- [ ] A merge rebases only the open agents whose claimed or edited files it touched.
- [ ] A PR that falls behind `master` is updated by PR duty without waiting for its author, and the author's commits are never rewritten.
- [ ] A conflict at landing reaches the owning agent within one event cycle and is resolved without user input.
- [ ] A comment from a PR duty agent reaches the authoring thread automatically and is acted on without user input.
- [ ] The thread is not finished until `pr://merged`.

### F7 · Duty agents

Long-running roles. They appear under **On duty**, with an agent count (×N) and a one-line summary. Start one from the home chips (`V`, `S`), the palette, or the + on the group. The start dialog sets role, number of agents (1–4), model and role-specific rules. Starting a role that already exists adds agents to it.

**Duty view:**
- **Rules:** chips. Hard rules are shown locked.
- **Agents:** one card per agent with its model and what it is doing now.
- **Activity:** a timestamped feed.
- **Header actions:** Pause/Resume, + Agent, Rules.
- **Composer:** instructs every agent in the role.

**PR duty:**
- Claims unclaimed PRs, one PR per agent, so two agents never review the same one. The review claim goes through the AIRC lock (`airc_claim` on the PR), so it is atomic. Only its holder updates, approves or merges that PR. A finding that arrives after the PR merged is filed as an issue.
- Runs the local checks, reviews the diff against SPEC.md and the Task List, leaves inline comments, requests changes or approves, then merges.
- **Local checks decide, not GitHub CI.** PR duty runs the project's checks in its review worktree, on the PR merged onto the current `master`: format, build with locked dependencies, lint with zero warnings, the UI checks, and `lake build` with the axiom audit when `proofs/` changes (the commands come from AGENTS.md). When they pass, the PR can merge.
  - GitHub CI is advisory. A pending, red or stale run never holds up a PR whose local checks pass.
  - **Why:** a merge queue stalled for an hour on CI that was red only because `master` had been broken. GitHub CI also costs money per run.
  - Testing the merge result, not the PR head, also catches a PR that broke only after another PR merged.
- **Rules:**
  - **Never merge without an approving review** (locked).
  - **Never merge unless the proofs check** (F17; locked).
  - Review every theorem statement against the claim and SPEC.md.
  - Local checks pass on the merge result before merge (locked). GitHub CI never blocks.
  - Check against SPEC.md.
  - Re-review on new commits. A merge commit PR duty pushed to update the branch is not a new commit to review; only its conflict resolutions are.
  - Update branches with `master` (F6): allowed and encouraged, never by force-push.
  - Delete the branch after merge.
  - Merge method: Squash / Rebase / Merge commit.
- Each review runs in its own worktree (F5).
- **How a review runs.** Every 30 seconds, and at once when PR duty starts, resumes or gets agents, each idle PR duty agent claims the oldest open, ready PR that needs work. A head with no decision gets a review; a head with a standing approval gets its checks and merge; a head with changes requested, or whose approved merge was refused, waits for its author to push. A head an agent already finished a session on without a decision isn't claimed again until it moves, or until you instruct PR duty.
  - The claim posts a `claim` event and a `Reviewing · <nick>` comment on the PR, which the rail reads. Until `airc_claim` lands (M9), the review runner holds the lock itself; the agent's session ending posts `released`.
  - A review is one agent turn in the PR's `review-pr-<n>` worktree: a detached checkout of the PR head with the base branch's tip merged in, so the checks run on the merge result. If that merge conflicts, the checkout stays at the PR head and the agent resolves the conflict or sends it back (F6).
  - The agent records decisions only through Velocity's PR duty MCP tools, served by Velocity's own executable: `pr_duty_pull_request` reads the PR, `pr_duty_review` approves or requests changes at the head it checked (the approval record above), and `pr_duty_merge` merges the approved head with the role's merge method. The launcher fixes the agent's nick and its one PR, so it can't act on another agent's PR or record a decision as anyone else.
  - Its card shows `#n · <current step>`; the feed gets the claim, each approval, request for changes, merge or refused merge, and a review that ended without a decision. A merged PR's review worktree is removed (Rule 5).
  - A message from the PR duty composer goes to each running review as its next message, and to each idle agent with its next claim.
- **Approval record** *(proposed; answers Q28)*. Velocity records each approval itself, as an `approved` AIRC event naming the PR, the reviewer's nick and the head SHA it approved. That event is the approval Rule 4 requires.
  - GitHub's review state is advisory, like CI. Every agent shares your one GitHub login, which can't approve its own PRs, and Velocity never asks for a second account's credentials or an app key (F15).
  - PR duty merges only the head it approved (`gh pr merge --match-head-commit <sha>`), so a commit pushed after approval needs a new review.
  - A branch update PR duty made itself with no conflict resolution moves the approval to the new head (it is not a new commit to review, see Rules below). Any other push clears it.
  - A `changes` event newer than the approval blocks the merge. PR duty mirrors each record on GitHub as a comment ("Approved at `<sha7>`") so people reading the PR see it.
  - On a repository whose branch protection requires a GitHub approval, the merge fails and Velocity posts a `failed` event. It never bypasses the rule (`--admin`).
- **Review the intent, not each line.** The reviewer checks the PR against its claim, its Task List item, issue or SPEC.md section, and AGENTS.md. Style is left to linters.
- **Severity bar** (locked). A finding blocks a merge only for: data loss or corruption, a crash or hang, a §1 rule violation, a security hole, a failing local check, or the main path of the claimed piece not doing what its SPEC.md section says.
  - Everything else (edge cases on rare inputs, display miscounts, naming, polish) is filed as an issue with its evidence, linked from an approving review, and goes back to the pool for scouts and other agents.
  - Every blocking finding goes in the first review. Later rounds review only the commits pushed since, and block only on findings in those commits or on new data loss or a crash.
  - When a reviewer is unsure whether a finding blocks, it does not block.
  - **Why:** one PR took 14 hours and five review rounds, each blocking on a narrower display edge case than the last, while the PRs queued behind it fell into conflict with every merge.
- **Review the statements, not the proofs.** Lean has already checked every proof. The reviewer's job is whether the right thing was proven (F17).
  - Does each statement say what the SPEC.md section says?
  - Do its hypotheses exclude real inputs?
  - Is the cost model honest, and is the class of alternatives the one that matters?
- **PR duty writes tests too.** When a reviewer doubts a claim a proof doesn't cover, it writes a stress script, runs it, and attaches the result to its comment.
- **Tiered review** *(proposed)*. Velocity tiers each PR from its diff:
  - **Trivial:** docs, comments or formatting. No proof needed. Approved once the local checks pass and the change matches its claim.
  - **Normal:** everything else. Full review.
  - **Sensitive:** touches any path listed under "Sensitive paths" in AGENTS.md (e.g. migrations, worktree deletion, CI config, auth). Full review, plus the full stress suite before merge, plus a confidence proof at the strongest level the language allows (F17).

  The tier shows on the rail PR row. Every tier still needs an approving review, so "never merge without approval" holds.
- **Landing ticks the plan.** When PR duty lands a PR for a §7-style checklist item, it ticks the item in the same branch update it makes before merging. Authors never tick it, so PRs don't all edit the same checklist lines.

**Main watcher** *(role name proposed)*: a duty role that watches `master` after merges, modelled on Anthropic's "superfan" agents.
- After each merge it runs every scout test and stress script (F17). Pre-merge CI is only build and lint, and the proofs are checked locally.
- When `master` breaks, it bisects the recent merges, posts a `broken` event naming the merge, and opens either a fix PR or a revert PR. That PR goes through PR duty like any other; the watcher never pushes to `master`.
- Targeted delivery (F14) tells only the agents whose branches contain the breaking merge.
- It tracks flaky tests and files issues for them.
- Rule: **Never push to `master`; every fix or revert is a PR** (locked).

**Scouts:**
- **Assume every part of the application is broken and will fail, then prove it.** Scouts write the project's tests and stress scripts (F17). Building agents don't.
- Sweep the repo: run the stress scripts against the code and against the Lean reference models, run lint, read SPEC.md and the proofs, and file issues.
- Focus areas: Bugs, Stress, Proof gaps, Spec drift, Performance, Security, Dead code, UX.
  - **Stress:** load, concurrency, malformed input, resource exhaustion, crash and restart.
  - **Proof gaps:** features with no proof, statements weaker than SPEC.md, hypotheses that exclude real inputs, and languages stuck at the model level.
- **Rules:**
  - **Never file duplicates** (checks open and closed issues; locked).
  - **No issue without evidence:** a failing test or stress script, attached (locked).
  - Label issues for dispatch.
- Sweep: Once / Every 30 m / After every merge.
  - Each scout sweeps once when it joins, then on this cadence. "After every merge" sweeps when the base branch's tip on the remote moves. Pausing lets running sweeps finish and starts none.
  - A sweep is one agent turn in the scout's own `<scout>-sweep` worktree (F5), a detached checkout of the base branch's tip, reset before each sweep. Its card shows the agent's current step; the feed gets the sweep's start and end and each filing.
  - Scouts file only through Velocity's scout MCP tools: `scout_issues` (every open and closed issue, which the scout compares its finding against) and `scout_file` (runs the reproduction, which must fail, attaches the script and its output, refuses duplicates, and signs the issue with the scout's name).
  - A message from the Scouts composer goes to every running sweep as its next message; idle scouts start a sweep that follows it.
- Issues they file show their name in the Issues panel.

**Accept when:**
- [ ] Pausing finishes in-flight reviews and claims nothing new.
- [ ] Rail PR rows show the review state and the claiming agent.

### F8 · Task List

The project-scoped list in the top-right panel.
- **States:** draft (before spec lock), todo, now (an agent is on it), done (its PR merged).
- **The counter** shows `done/total`.

**Adding tasks (decided):**
- **You:**
  - an always-visible "Add a task" input pinned under the list (Enter adds and keeps focus)
  - `T` from anywhere
  - the + in the panel header
  - `/task <title>` in any composer
  - the palette
- **Agents:** they have a `task_add` tool and use it **only when your message asks them to** (e.g. "add these to the task list: X, Y"). Otherwise they file issues. This rule is written in AGENTS.md.
- Tasks not created by the spec show who added them: "added by you", or "added by <thread>", which links to that thread.
- Tasks added before the spec is locked are drafts.

**Live state:** a task with an agent shows that agent's state (agent working / #n · in review / merged · #n), linked to its thread.

**Accept when:**
- [ ] Re-rendering the list never takes focus from the add-task input.
- [ ] New tasks scroll into view.

> The full meaning of the Task List is still to be explained by the user (§8). Do not add task fields or behaviour beyond this section.

### F9 · Issues and dispatch

- **The Issues panel** lists open GitHub issues: number, title, label and the scout that filed it.
- **Questions** *(proposed)*: issues labelled `spec-question` are open SPEC.md §8 questions an agent ran into. They show as their own group at the top of the panel, sorted by how many pieces each one blocks. Each row shows the filer's recommended answer.
  - **Accept** opens a one-line SPEC.md PR that records the answer as *(proposed)*. PR duty lands it as Trivial (F7), and the blocked pieces become claimable.
  - Agents never wait on a question: they file it and claim other work (Rule 1).
  - **Why:** unanswered questions, not code, were what kept most of a milestone unclaimable.
- **Dispatch** (hover button, `I`, or the palette): starts one agent on the issue, in its own worktree, with the normal lifecycle. The issue stays listed with its agent's live state until the PR merges, which closes it.
  - `I` opens the palette filtered to issues. Dispatched threads keep the issue title and use `vel/issue-<n>` (with a unique suffix when needed), without calling the title model. Their saved issue identity includes the repository; repeat dispatch opens the owning thread. Deleting that thread frees the issue after Undo expires (F12).
  - A question's recommended answer is read from a `Recommended answer:` or `Recommendation:` paragraph; its `Blocks:` bullet list supplies the blocked-item count. Questions without a recorded recommendation stay readable on GitHub. Accept starts the normal agent lifecycle with a prompt limited to the one-line proposed SPEC answer and its PR.

### F10 · Swarm

One agent per open item, all started at the same time, each in its own worktree. Open it with the **Swarm** buttons on the Task List and Issues panels, `W`, the home chip, or the palette.

**Swarm dialog:**
- **Source:** Task List · N, Issues · N, or Both · N.
- **Items:** checkboxes, all open items checked by default, with Select all/none. Items that already have an agent, and draft tasks, are shown but can't be selected.
- **Models:** choose one or several. Several are assigned round-robin, and the split is shown ("3 on Opus · 3 on Sonnet · 3 on Codex").
- **Rules:**
  - Each agent opens a PR and watches it until it merges (locked, from AGENTS.md).
  - Each agent gets its own worktree under the configured location (locked).
  - Rebase only when a merged PR touches your files, otherwise once at landing (locked, F6).
  - Close each issue when its PR merges (default on).
- **PR duty capacity:** aim for about 1 reviewer per 3 open PRs. If PR duty is short, offer "Add N agents to PR duty"; if nobody is on PR duty, offer to start it. Without a reviewer, PRs wait as "needs a reviewer".
- **Footer:** "N worktrees on D: · all start now". Launch with `Ctrl+Enter`.

**Swarm in the sidebar:** one row under **Swarms**, with ×N, a segmented progress bar (working / in PR / fixing / merged) and counts. Member threads are listed under it, indented, only while the swarm or one of its members is selected. They are not listed in the other groups.

**Swarm view:**
- **Header:** N agents · N worktrees live · start time · merged/total. **+ Items** adds open items to this swarm.
- A big progress bar plus a legend.
- **Agent cards:**
  - the item (task, or issue #n)
  - a 5-dot track: work · PR · CI · review · merged
  - what it is doing now
  - agent id, model and +/−

  Click a card to open that agent's thread.
- **Activity:** PR opened, changes requested, comment resolved, merged (task done / issue closed), "N affected agents rebased after #n", conflicts, stalls, and "Swarm finished · all N PRs merged".
- **Composer:** a message goes to every agent still working; each picks it up at its next step.

**Merging a member** ticks its task done or closes its issue, removes the PR from the rail, deletes the worktree, and rebases only the members whose files it touched (F6).

**Accept when:**
- [ ] All selected agents start immediately, unless a provider concurrency cap is set (F15, off by default; §8 Q4).
- [ ] Deleting a member returns its task to todo, or frees its issue.

### F11 · AGENTS.md editor

Open it from the rail link or the palette. It reads and writes the repo's `AGENTS.md`.

- **An outliner, not a text editor:**
  - sections (`##`) with bullet rules and one level of sub-rules
  - inline code shown as code (backticks visible only while editing)
  - saved as plain markdown
  - a **Markdown** toggle shows the exact file agents read
- **Enter submits a bullet:**
  - A fast model *(proposed: Haiku 5.5)* rewrites it as a direct rule, using the whole file as context (e.g. it reuses the commands from `## Commands`).
  - The edit lands at once, with no approval, and the cursor moves to a new bullet. You keep typing while it works.
  - It can:
    - **tighten** ("You should probably keep functions under 60 lines." → "Keep functions under 60 lines. Split anything longer.")
    - **expand** short notes ("no unwrap" → "Never call `unwrap()` or `expect()` outside tests.")
    - **split** a bullet with two rules into two
  - If it cannot work out what a vague word means, it leaves the bullet flagged and says what is missing ("say what 'clean' means").
  - Every rewritten bullet shows "was ~~original~~ · **Undo**" until you save.
- **Live lint while typing:** marks hedges (should probably, maybe, try to, where possible, as needed, consider), "should", and vague words (clean, good, nice, appropriate, reasonable, properly, etc.).
- **Side panel:**
  - **Covers:** commands (build, proofs, lint); implementation first; proofs: proof list only, after the code; tests and docs: low priority; scope; PR: monitor until merged; PR: address every review comment; definition of done; hard "never" list. Each missing item has **Add**, which inserts editable bullets into the right section.
  - **Flagged:** the list, with **Tighten all**.
  - a key legend
- **Keys:**

  | Key | Action |
  |---|---|
  | `Enter` | submit |
  | `Shift+Enter` | new bullet, kept exactly as typed |
  | `Tab` / `Shift+Tab` | indent / outdent |
  | `Alt+↑/↓` | move bullet |
  | `Backspace` on an empty bullet | delete it |
  | `↑/↓` at the start/end of a bullet | move to the previous/next bullet |
  | `## Name` + `Enter` | new section |
  | `Ctrl+S` | save and commit |

- **Leaving a bullet** you changed also submits it.

**Default installation:** New project setup installs the F11 template in the main checkout when `AGENTS.md` is absent and commits that file alone before registering the project and starting its interview. Existing files (including empty files), directories and symbolic links are preserved. The template is installed atomically, so concurrent setup cannot replace instructions. Installation or commit failures stop setup and report the error; created instructions remain available for recovery. The interview continues to write only SPEC.md (Q7). The template includes implementation-first, proof-list-only, scope, PR follow-through, Task List, AIRC, shared-file and review-severity rules. Project-specific commands are filled in from the project's manifests and spec rather than assuming Rust or a frontend stack.

**The default AGENTS.md** includes these rules:
- Opening the PR does not finish the task; monitor it until it merges.
- Address every PR duty comment.
- Add to the Task List only when the user asks; otherwise file an issue.
- Review severity: what blocks a merge, and that everything else becomes an issue (F7).
- Shared files: the project's generated files and registration lists, and how to merge them (F14).

**Accept when:**
- [ ] Saving commits AGENTS.md; agents pick it up on their next task.
- [ ] Undo restores your exact original wording, including removing any split-off bullet.

### F12 · Thread context menu and delete

Right-clicking a thread in the sidebar opens a menu:
- Open worktree in Explorer
- Copy worktree path
- Open PR #n on GitHub
- **Delete …** (shown in red)

The delete label depends on the thread:
- **Delete thread and worktree**, with what it will do underneath: "stops the agent · closes PR #n · 247 MB freed"
- **Delete swarm and its N worktrees** (swarm row)
- **Delete thread** (merged threads, whose worktree is already gone, and spec threads)

Duty threads have no delete entry.

Delete is **immediate, with no confirmation** (decided).
1. The thread disappears and a toast offers **Undo** for 5 s.
2. When the 5 s are up:
   - the agent is stopped;
   - the worktree is deleted;
   - any open PR is closed;
   - a linked task goes back to todo, or a linked issue becomes free again.

**Accept when:**
- [ ] Undo within 5 s restores everything exactly.
- [ ] After 5 s nothing remains on disk.

### F13 · Settings

Open with the footer gear, `Ctrl+,`, the palette, the footer worktree count (→ Worktrees) or the footer limit meter (→ Providers). Pages: **Worktrees** (F5), **Providers** (F15: sign-in, default model, effort and speed for new threads, and the thread title model), **Skills** (F18), **Agents** and **Proofs**. Proofs holds the project's proof list, the code-level translator for each language when an issue asks for one (Rust: Aeneas), and the stress-diff input count (F17). Agents holds the shared MCP servers passed to every agent (§3.2 Tool access), the per-agent caps (max turns, max duration; off by default), the stall timeout (5 min) and the claim-release grace period (10 min) from §2.2. The rest of the Agents page is unspecified. There is no Keys page: every provider signs in with OAuth.

### F14 · AIRC (claims, events, agent messages)

One IRC-style channel per project, `#<project>`, shared by every agent in it: thread agents, swarm members, dispatched agents and duty agents. It exists so agents coordinate instead of working in an echo chamber: they see each other's work, catch feature overlap early, and sequence work that depends on another agent's PR. It is not meant to be a chat room. What fills it is **events**, which Velocity posts from what it observes. **Messages** between agents are allowed but are for the cases events can't express. Modelled on xros_chat (daily logs, nicks, presence, mentions, ledgers, MCP tools).

**Nicks:** the worktree leaf (`thread-ipc`, `command-palette`, `issue-57`), at most 16 characters, cut at a word boundary. Duty agents use their role nick (`pr-1`, `scout-1`). Goal swarm agents use their model's short name and a number (`sol-1`, `argon-2`, `fable-1`), counted per project. Velocity posts as `velocity`. You post as `you`.

**Events.** Each event is one line: time, a type tag, the nick, and what happened. Velocity posts them; agents never type them.

| Tag | Posted when | Comes from |
|---|---|---|
| `claim` | an agent's claim is granted, or it extends a claim (e.g. adds an issue its PR also fixes) | `airc_claim` |
| `refused` | a claim loses to an earlier one; names the holder | `airc_claim` |
| `overlap` (amber row) | two working or PR-open agents claim the same file | Velocity's claim comparison |
| `shares` | an agent will make a small change in another agent's file and rebase after it merges | `airc_share` |
| `waits` / `unblocked` | an agent waits for another agent's PR, then rebases after it merges | `airc_wait`, `pr://merged` |
| `PR` · `changes` · `pushed` · `approved` · `merged` | the PR lifecycle (F6, F7) | GitHub; `approved` is PR duty's approval record (F7) |
| `order` | PR duty sets a merge order | PR duty |
| `issue` | a scout or agent files an issue | GitHub |
| `task` | a Task List item is added (by you or by an agent, F8) | Task List |
| `failed` | an agent stops on an error; its claim is held | agent runner |
| `discovery` | a Discoveries ledger entry is added | `append_ledger` |
| `swarm` | a swarm or goal swarm starts | you |
| `left` | an agent leaves without a PR (e.g. nothing left to claim) | agent runner |
| `rebased` | a merge touched an agent's files and it rebased (F6) | Velocity |
| `updated` | PR duty merged `master` into a PR's branch; names the PR and its new head (F6) | PR duty |
| `conflict` | a PR's update onto `master` conflicts in a way only its author can resolve; names the files and the owning agent (F6) | PR duty |
| `stalled` · `at limit` · `released` | activity changes and claim release (§2.2) | Velocity |
| `broken` · `reverted` | `master` broke after a merge, and its fix or revert (F7) | Main watcher |
| `proved` | a design or confidence proof checks, with its level per language (F17) | Velocity |
| `statement` | a theorem statement changed after review; the review resets (F17) | Velocity |
| `stress` | a scout's or reviewer's stress script fails; the evidence for an issue or comment (F17) | scout, PR duty |

**What agents do** (enforced by the AGENTS.md "Coordination (AIRC)" section, F11):
- **Claim before editing.** Claim the task, branch and every file they expect to touch. Claims on a piece of work go through Velocity's lock (`airc_claim`): the first claim wins, and a later claim is refused with the holder's nick, so the refused agent picks other work. The claim stays until the PR merges or the thread is deleted. A failed or closed thread keeps holding its claim.
- **Shared files are not contested.** AGENTS.md lists them under "Shared files":
  - **Generated files** (lockfiles, generated bindings) are never claimed. They are regenerated after every merge with `master`, never merged by hand.
  - **Registration lists** (the command, event and module lists every feature adds a line to) take shared claims, which never raise an overlap.

  Without this, every agent overlaps every other on the same few files, and targeted rebases (F6) turn into rebasing everyone.
- **Record overlaps instead of discussing them.** When Velocity posts an overlap, the later agent records either a share (`airc_share`) or a wait (`airc_wait`) before it edits the shared file. In a wait, its thread shows "waiting for <nick> · #n · shares <file>" and makes no edits. When the PR merges, it rebases and an `unblocked` event is posted. If the PR is closed, fails or is deleted, the wait ends.
- **Message only when needed.** A message is for something events can't say: asking the file's owner for a change, a handoff note after a merge, or answering a mention. Agents never restate an event in a message.
- **Address every message.** A message names its recipients (`to`) and its `intent`: `inform`, `request` or `question`. An agent never replies to an `inform`. It answers a `question`, and replies "done" or "declined, because …" to a `request`. Agents can't broadcast; only your comments go to every agent.
- **Log discoveries** that other agents need (environment quirks, platform bugs) to the Discoveries ledger. Every merge is appended to Past work.

**Targeted delivery** (from Google Scion's "Not in `to` → not for you"). At each step boundary, an agent receives only:
- events about its own claims, PR, waits and shares;
- overlaps and refusals that name it;
- merges whose files touch its claimed or edited files;
- `broken` events for merges its branch contains (F7);
- messages with it in `to`;
- your comments.

Nothing else is pushed to it. The full log stays readable on demand (`read_events`, `airc_claims`), for example while choosing work in a goal swarm (F16). This keeps agents heads-down and stops reading costs growing with the square of the agent count. The modal always shows you everything.

**Message modes** (per agent, set from the nick list or the thread's context menu):
- **Open** (default): receives what targeted delivery sends it.
- **Heads-down:** messages from other agents are held until it opens or updates its PR, or goes idle. Events and your comments always get through.
- **Quarantined:** receives nothing from other agents, its messages are delivered to nobody, and its claims are frozen (not released, not extended). You can still message it. Use it for an agent that is misbehaving.

**Your lines are comments, not prompts.** They are messages. A line starting `nick:` is delivered to that agent's thread (shown as "via AIRC") and read at its next step, never mid-step (Rule 1). Other lines are read by every agent at its next step.

**AIRC modal** (footer link or `A`; `Esc` closes):
- **Header:** `AIRC #<project>` and the channel topic.
- **Tabs:**
  - **Events** is the default tab. It shows the event log: `hh:mm`, a coloured type tag (blue for PR / issue / task, green for approved / merged / unblocked, amber for overlap / waits / shares / changes / order, red for refused / failed), then the nick and the text. Nicks have stable colours, and PR numbers are links.
  - **Claims** lists one row per claiming agent: nick, task, state, and its files. Overlapping files are outlined amber. A dependency shows "waits for <nick> (#n) to merge". The tab label shows the overlap count.
  - **Messages** shows agent-to-agent messages and your comments, as `<nick> text`. Mentions of you are highlighted. Sending a comment switches to this tab.
  - **Discoveries** and **Past work** are the two ledgers.
- **Nick list:** grouped On duty / Choosing work / Working / PR open / Holding a claim / Left (last 4). Each nick shows what it is doing. Clicking a nick starts a `nick: ` line.
- **Input:** `<you>` and a one-line input. Enter sends.

**Storage:** daily log files and ledger files per project under the app data folder. Agents reach AIRC through MCP tools (`claim`, `release`, `share`, `wait`, `read_feed`, `read_events`, `send_message { to, intent, text }`, `read_messages`, `append_ledger`, `read_ledger`), which the agent runner gives every agent on every provider; the agent runner injects unread mentions at each step boundary.

The M8 channel backend is `velocity-airc`: launch `velocity-airc <app-data> <project-id> <member-id> <worktree-leaf-or-duty-nick>` as an MCP stdio server. Initialization joins the channel; EOF leaves it. Project identities partition storage independently of display names. The project-id is the project's store identifier. Velocity posts events (the F14 tags) as `velocity` on behalf of the nick they name, without joining; an event about a PR also carries its number and head SHA, which the approval record (F7) reads. UTC daily JSONL logs and the Discoveries/Past work files share monotonically increasing sequence cursors; writes are serialized across processes and flushed before acknowledgement. Nicks are cut at word boundaries to 16 characters, with collision suffixes; `you` and `velocity` are reserved. Tools available at this stage are `join`, `leave`, `list_nicks`, `read_feed`, `read_events`, `read_messages`, `send_message`, `append_ledger`, and `read_ledger`. The channel also exposes `airc_claims`, `airc_claim { task, branch, files }`, `airc_release { task }` and `airc_share { holder, file }`. Claims are durable first-wins records under the channel's cross-process lock. Refusals name the holder and original time. Leaving, failure and closed PRs retain claims. Files use repository-relative paths with slash and dot-segment normalization; generated files are omitted. Registration-list claims use a `::registration` suffix so the rest of the source file remains contested. New overlaps post `velocity` lines naming both agents; `airc_share` records a share. Build threads claim their initial task and branch before ACP starts, receive the embedded AIRC MCP server, and extend claims from explicit file writes and Edit/Delete/Move tool locations. Partial updates inherit the last known kind and locations; Read/Search/Execute/Other and unclassified tool locations do not extend claims. Successful merge cleanup and final deletion release their claims. Dependency waits and the modal remain later implementation items.

M8 message delivery registers live build-thread members as `thread-<thread-id>` with worktree-leaf nicks. Native delivery passes named messages and your broadcast comments as `via AIRC` through ordinary queued steering at the next completed ACP turn. A per-thread cursor in `state.json` advances only after the receiving turn succeeds. Pending input replays after restart; acknowledged input does not. Events stay readable on demand until targeted event delivery ships. The later modal uses `airc_send` for comments.

**Runtime injection.** Every session Velocity starts, on every provider, gets an MCP server named `airc` in `session/new`: build threads (composer, dispatch and follow-through resume), scout sweeps, PR duty reviews and the spec interview. It runs Velocity's own executable as `velocity --airc-mcp <app-data> <project-id> <member-id> <nick>`, so the server ships inside the app; the standalone `velocity-airc` binary takes the same arguments, with or without the switch. Member ids are `thread-<thread id>`, `duty-<nick>` (scouts and PR duty) and `spec-<thread id>`. A thread's nick is its worktree leaf, a duty agent's its role nick, and the spec interview's `spec`.

**Acceptance:**
- Two agents that claim the same file get an overlap event within one step, and the later one records a share or a wait before it edits the file.
- PR, review, merge, issue and Task List events appear without any agent posting them.
- A normal run (claim → PR → merge with no conflicts) produces no messages.
- An agent is never sent an event or message unrelated to its claims, PR or waits unless it is in `to` or the line is your comment.
- A heads-down agent receives no agent message until its next PR update or idle, and a quarantined agent's messages reach nobody.
- A dependent agent makes no edits until its dependency merges, then rebases.
- Every merge produces a `merged` event and a Past work entry.
- Your `nick:` line reaches that agent's thread, and the agent replies in the channel.

### F15 · Providers (OAuth)

Four providers: **Anthropic** (Claude Code agent), **Codex** (Codex CLI agent), **Antigravity** and **Kimi Code** (Kimi CLI agent). Every one signs in with OAuth on your own subscription, the way T3 Code does. Velocity talks to each one's agent over ACP (§3.2) and uses that agent's own login. It never asks for, shows or stores an API key, and tokens stay in each provider's own credential store.

**Settings → Providers** has one card per provider:
- Its mark, its name, "runs <agent> over ACP · OAuth", and the ACP agent command (e.g. `claude-code-acp · stdio`).
- **Status:**
  - **Signed in** shows the plan and how many agents are on it.
  - **Waiting** shows "Waiting for you to finish signing in at <site>…" with Cancel.
  - **Not signed in** means the provider's models are hidden from every picker.
- **Sign in with Claude / ChatGPT / Google / Kimi:** opens the provider's OAuth page in your browser. When the login completes, the card flips to signed in by itself and a toast lists the models now available.
- **Sign out:** agents already running on that provider keep their session until they finish. New work can't use it.
- **Limit:** a bar showing the share of the current subscription window used (5-hour, daily, weekly, whatever the provider reports) and when it resets.
- **Max agents at once** (off by default): a cap on concurrently running agents for this provider.
  - Agents over the cap wait as "queued · <provider>" and start when a slot frees.
  - Parked agents (§2.2) don't hold a slot, so waiting on another PR never blocks new work.
- **Models:** chips that are read from the provider after sign-in. Clicking a chip hides that model from the pickers or shows it again.

**Default for new threads:** one model card per available model, plus effort and speed. A new thread starts on it. Spec interviews, duty agents and swarms choose their models in their own dialogs.

**Speed: Regular or Fast.**
- **Where available.** Some models offer a fast mode, which answers sooner, as Claude Code's fast mode does for Opus. Velocity reads whether a model has one from its agent over ACP (§3.2) and offers Fast only there.
- **Where it's set:** everywhere effort is chosen. That covers the composer menu, the defaults above, the thread title model, the new-project interview model, and each goal swarm mix row (F16). The duty and swarm dialogs use the default speed.
- **Models without a fast mode.** On those models the Speed control is greyed out and says "<model> has no fast mode". A thread set to Fast that switches to such a model runs at Regular, and goes back to Fast if it switches back.
- **Shown:** the model pill and agent cards add "· Fast" when it is on.
- **Limits:** if a provider reports that fast mode uses the subscription limit faster, the limit bar counts it that way (Q25).

**Thread titles:**
- **Default:** GPT-6 Luna · Low · Fast.
- **Settings:** model cards (signed-in providers only), effort and speed, and a "Reset to GPT-6 Luna · Low · Fast" button when changed.
- **When its provider is signed out:** the section says so, and titles use the first signed-in model at Low until it signs in again.

**Everywhere a model is chosen** (new project, duty dialog, swarm, goal swarm, composer menu, thread titles), only signed-in providers' models are listed. Each signed-out provider shows as a dashed "Sign in" card. Clicking it opens Settings → Providers and starts that provider's sign-in.

**Acceptance:**
- No screen anywhere accepts an API key.
- Signing in through the browser updates Velocity without a restart or a manual refresh.
- A signed-out provider's models never appear as selectable.
- A swarm can mix models from several providers, assigned round-robin.
- A goal swarm runs exactly the count you set on each model.

### F16 · Goal swarm

**What it is:**
- Your T3 Code routine as one action. Today you open 5–20 threads by hand and paste the same prompt into each: read AGENTS.md and start dev, push a branch, open a PR, watch it, keep the worktree on a given drive, remove it after the merge.
- A goal swarm starts all of those agents at once, with one prompt and the model mix you choose.
- It differs from a Swarm (F10) in one way: nobody assigns work. Each agent decides what to build, and AIRC (F14) keeps two agents off the same piece.

**Dialog** (`G`, the "Goal swarm" starter chip, or the palette):
- **Prompt:** every agent gets exactly this text. The default is `Read AGENTS.md and start dev.`; text typed in the empty composer prefills it.
- **Velocity adds to every agent** (locked):
  - its own worktree under the worktree root (F5), removed after its PR merges;
  - commit, push, open a PR against the default branch, and watch it until it merges (AGENTS.md);
  - join `#<project>` and claim work before editing, where the first claim wins;
  - prove a new feature's design in Lean before writing its code, and prove the code after (F17).

  These replace the lifecycle half of the hand-written prompt. The worktree drive comes from Settings → Worktrees, not from the prompt.
- **Agents:** a stepper from 1 to 30, with quick picks 3 / 5 / 10 / 20. All of them start at once.
- **Mix:** one row per model.
  - Each row has a model picker (signed-in providers only, F15), effort (Low / Med / High / Max), a Fast toggle (greyed out when the model has no fast mode) and a count.
  - The first row is "the rest". It absorbs the difference, so the rows always add up to the agent count.
  - Raising another row takes one agent from the rest; if the rest is empty, it raises the total.
  - Buttons: **Add a model** and **Even split**.
  - A bar above the rows shows the split in provider colours.
  - Next to the buttons is a preview of the nicks, for example `sol-1…6 · argon-1…2 · fable-1…2`.
  - Example: 10 agents = 6 GPT-6.1 Sol High + 2 Gemini 4 Argon High + 2 Fable 5.1 High.
- **Limits:** one line per provider in the mix: agent count, share of the window used, and the reset time when 60% or more is used (F15).
- **Pool:** "They choose from N unclaimed pieces: x Task List · y issues · z unbuilt SPEC.md sections." If there are more agents than pieces, it says how many will find nothing and leave.
- **Options:**
  - **Keep going** (off by default): after its PR merges, an agent claims the next unclaimed piece in a fresh worktree instead of stopping.
  - **PR duty capacity:** the same check as the Swarm dialog (one reviewer per 3 open PRs).
- **Launch N agents** (`Ctrl+Enter`).

**What each agent does:**
1. Gets a nick such as `sol-3` and a worktree named after it, then reads AGENTS.md and SPEC.md. State: **choosing work**.
2. Reads the AIRC claims board and picks an unclaimed piece. The pool is:
   - open Task List items, except drafts and items another agent is already on;
   - open issues;
   - SPEC.md sections nobody has built.
3. Claims it with `airc_claim`.
   - **Granted:** it renames its branch for the piece (`vel/<slug>`, or `vel/issue-<n>`), posts its claim with files, and continues.
   - **Refused** (another agent claimed the piece after this agent read the board): a `refused` event names the holder. The agent re-reads the board and claims the first open piece, which posts a `claim` event. No message is needed.
   - **Nothing left:** a `left` event is posted; the agent removes its worktree and leaves.
4. From here it is an ordinary thread (F4–F6). It settles overlaps and dependency waits in AIRC (F14), opens a PR, watches it until PR duty merges it, and its worktree is deleted. Merging ticks the task, closes the issue, or records the SPEC.md section as built in the project's state. Velocity records it at landing; the agent's PR never edits SPEC.md to mark it, so PRs don't all touch SPEC.md.
5. With keep going on, the same nick goes back to step 2.

Agents launched together read the board within seconds of each other, so several often want the same piece. The lock is what makes that safe. The losers pick again within one step instead of duplicating work.

**Sidebar:** one row under Swarms, with a target icon, "Goal · <prompt>", ×<agents> and the segment bar. Its members nest under it like swarm members, and each member is titled by the piece it claimed.

**Goal swarm view:**
- **Header:** title, "<n> agents · one prompt", start time, PRs merged. Buttons: **AIRC** and **+ Agents** (more agents on the same prompt).
- **Prompt bubble:** "goal swarm · the same prompt to all <n> · <mix>".
- **Segment bar:** one segment per agent (choosing, working, PR open, addressing review, merged, failed), plus "<n> waiting on another agent's PR".
- **Claims board:** every piece in the pool, in three groups:
  - claimed pieces first, with the holder's nick and state (working, "waits for <nick> · #n", "#n · in review", failed · claim held);
  - open pieces next;
  - pieces this swarm merged last, struck through, with "merged #n".
- **Agents:** one card per agent showing nick, model, effort (and "Fast" when on), claimed piece, the claim → PR → CI → review → merged track, and what it is doing. Clicking a card opens that thread.
- **Activity:** claims, refusals, PRs, waits, merges and leaves.
- **Composer:** messages every agent in the swarm (read at its next step, Rule 1).

**Acceptance:**
- One action starts N agents with the exact mix and efforts chosen. Each runs in its own worktree on the configured drive.
- No two agents ever hold a claim on the same piece. A refused claim shows in AIRC, and that agent claims something else within one step.
- Every agent's work ends in a merged PR (or a held claim on failure), and its worktree is deleted on merge.
- With more agents than pieces, the extras leave without making a worktree change or a PR.
- Keep going: an agent that finishes takes the next open piece until the pool is empty.

---

### F17 · Proofs and adversarial testing

**Why:** assume everything fails. Tests written by the agent that wrote the code cost CI money, slow the builder down, and check what the builder already believed. So the work is split:
- **Builders prove.** Lean proves the design is right and the code matches it.
- **Breakers test.** Scouts and PR duty, whose job is to find failure, write every test and stress script.

Lean is the proving ground, not the test suite.

| Who | Writes | Never writes |
|---|---|---|
| Building agents (threads, swarms, goal swarms) | Code first; then proofs for proof-list features, and a targeted test only for a critical contract | Test suites, stress scripts, Lean for anything off the proof list |
| Scouts | Tests, stress scripts, issues with evidence | Fixes |
| PR duty | Reviews of theorem statements, stress scripts to back a doubt | Proofs for the author |
| Main watcher | Nothing new. It runs every test and stress script after each merge. | |

**Which features get proofs.** Only the features on the project's **proof list**: design decisions where a mistake loses work, corrupts data or breaks coordination, and where there is a real choice between designs.
- **On the list:** algorithms, concurrency and coordination, data integrity, and anything destructive.
- **Never on the list:** UI, layout, styling, configuration, persistence formats, IPC, provider sign-in and toolchain glue. These ship with no Lean.
- **Who sets it:** the spec interview (F3) proposes the list and SPEC.md records it. Adding a feature takes your decision; an agent that thinks one belongs files an issue.
- **Why:** proving every feature turned Lean into a test suite. On Velocity's own first milestones, Lean grew to 43% of the repo, with theorems about single keyboard shortcuts and token values, while implementation stalled.

Velocity's own proof list:

| Feature | SPEC | What could go wrong |
|---|---|---|
| Claim lock | F14 | two agents hold the same piece or file |
| Targeted rebases and landing | F6 | an affected agent isn't rebased, or a merge drops a change |
| Worktree deletion | F5, F12 | a worktree with unpushed work, or another thread's, is deleted |
| Goal swarm work choice | F16 | a piece is taken twice, or an agent leaves while a piece is unclaimed |
| Stall detection and claim release | §2.2 | a live agent's claims are released, or a dead agent's never are |

**Implementation first.** Proofs never block building. The agent builds and runs the feature, then writes its proofs in the same PR. Implementation stays the larger part of every PR: proofs, tests and docs together are smaller than the code, and PR duty requests changes when they aren't.

**Design proof** (`proofs/<Feature>/Design.lean`, after the code, in the same PR, for every feature on the proof list):
- **Exactly three theorems:** `correct`, `cost_bound` and `optimal`, below. Lemmas are allowed only as steps toward them.
- **No theorem about a single behaviour, case, key, value or example.** That is a test, and PR duty requests changes.
- **Size tripwire:** a design proof over 300 lines means the model is too detailed. The agent stops and files an issue.
- **Model:** the feature as Lean definitions: its state, its operations and the SPEC.md section it implements, stated as a `Spec` predicate.
- **`correct`:** the design meets `Spec` on every input.
- **`cost_bound`:** its cost stays within a bound, under a cost model the file defines (steps, allocations, rebases, round trips, whatever the feature spends).
- **`optimal`:** no alternative in a stated class meets `Spec` at a lower cost.
- **"Could not be any better" is proven against something stated.** Optimality only holds for a named cost model and a named class of alternatives. A proof against an unstated alternative proves nothing, so the statement must name both. "Cleanest" isn't a theorem; the reviewer judges it.

Example, from Velocity itself (F6):

```lean
-- proofs/TargetedRebase/Design.lean
theorem correct : ∀ m bs, ConflictFreeAtLanding (targeted m bs)
theorem cost_bound : ∀ m bs, rebases (targeted m bs) ≤ (bs.filter (touches m)).length
theorem optimal : ∀ strat ∈ RebaseStrategies,
    (∀ m bs, ConflictFreeAtLanding (strat m bs)) →
    ∀ m bs, rebases (targeted m bs) ≤ rebases (strat m bs)
```

**Confidence proof** (`proofs/<Feature>/Confidence.lean`, after the code): proves what was built matches the design. Each language in the change reaches the strongest level available to it:

| Level | How | Available for |
|---|---|---|
| **code** (on request) | A translator turns the code into Lean, and the proof shows the translated code refines the design model. | Any language with a translator set for the project. Rust ships with Aeneas (its safe subset). More translators are added per language in Settings → Proofs (Q23). |
| **reference** (default) | The Lean model exports a compact fixture table, and one test checks the code against it. For larger input spaces, the model is compiled (Lean 4 compiles to C) and exported with `@[export]`. Scouts' stress scripts run the code and the model on the same inputs and diff the outputs. C and C++ link the model directly. Python, TypeScript and every other language call it through their FFI or a small process. | Every language |
| **model** | Only the design is proven. The PR is flagged, and scouts treat it as a proof gap. | Every language |

**Generated translations** (Aeneas output) are marked generated in `.gitattributes`, so they stay out of language statistics and PR diffs.

**Bug fixes:** no design proof. In a proof-list feature, the confidence proof must cover the input the scout's evidence failed on. Elsewhere, a fix has no Lean. The fixing agent copies the scout's script into `stress/` unchanged as part of its PR. Copying isn't writing: Velocity checks the file matches the issue's attachment byte for byte.

**What Velocity checks** (in the PR's worktree, before PR duty sees it, and again on every push):
- `lake build` succeeds for the PR's proof files.
- No `sorry` or `admit`.
- `#print axioms` lists only `propext`, `Classical.choice` and `Quot.sound`. This also catches `native_decide`, which adds an axiom of its own, and any `axiom` declaration.
- **Statement lock.** When PR duty approves, Velocity hashes every theorem statement. If a statement changes afterwards, the approval is withdrawn, the review restarts and a `statement` event is posted. Weakening a statement until its proof goes through is the cheat this catches.
- The design proof names the SPEC.md section it covers.

**Merge gate:**
- PR duty can't merge until the proofs check (locked, F7).
- A missing design proof on a proof-list feature blocks the merge.
- Lean for a feature off the list, or a theorem about a single behaviour, gets changes requested.
- A confidence proof at the model level for a language that has a stronger level available is flagged on the proof card and left to the reviewer. Sensitive paths need the strongest level (F7).

**Where it runs:**
- Proofs and stress scripts run locally, in each agent's worktree, on the machine that already runs the agents.
- GitHub CI runs build and lint only, and is advisory: it never blocks a merge (F7).
- Tests and stress scripts live in `stress/` (and the project's own test folder). Scouts run them on every sweep and the main watcher runs them after every merge. They never run in per-PR GitHub CI.

**Proof card** (in the PR history, F6):
- the proof folder, and "N theorems proved";
- the four checks, each ticked: lake build; no sorry or admit; standard axioms; statements locked at review;
- every theorem: Design or Confidence, its name and its statement;
- the level reached per language, e.g. "Rust · reference · fixture table checked by one test", "TypeScript · reference · scouts diff 10,000 stress inputs against it".

**Accept when:**
- [ ] A building agent's PR adds tests only for critical contracts, and its implementation outweighs its proofs, tests and docs together.
- [ ] A proof-list feature's PR can't merge without a design proof that checks.
- [ ] A PR for a feature off the proof list contains no Lean.
- [ ] A PR whose proofs use `sorry`, `admit`, `native_decide` or a new axiom can't merge.
- [ ] Changing a theorem statement after approval withdraws the approval and posts a `statement` event.
- [ ] Every issue a scout files carries a failing test or stress script.
- [ ] The proof card shows each language's level, and a model-only level is flagged when a stronger one exists.
- [ ] Per-PR GitHub CI runs only build and lint.
- [ ] A PR whose local checks pass on the merge result merges while its GitHub CI run is still pending or red.

### F18 · Skills

**What it is:** first-class support for skill packs: installed once, kept on a pinned commit, switched on per project, and given to every agent of every provider. Four packs are built in. Any other repo that follows the Agent Skills standard can be added.

| Pack | Repo | What it is | Shape | Needs |
|---|---|---|---|---|
| **UI UX Pro Max** | [nextlevelbuilder/ui-ux-pro-max-skill](https://github.com/nextlevelbuilder/ui-ux-pro-max-skill) (MIT) | Design intelligence: 80 styles, 192 palettes, 74 font pairings, 25 chart types, 120 UX rules, guidelines for 22 stacks, behind a local BM25 search (`scripts/search.py`). | 1 skill, CSV data, stdlib Python | Python 3 |
| **claude-skills** | [alirezarezvani/claude-skills](https://github.com/alirezarezvani/claude-skills) (MIT) | 410 skills in domain bundles (engineering, product, marketing, research, regulatory, C-level, finance and more), with 807 stdlib-only Python tools and reference docs. | many skills, in bundles | Python 3 |
| **Game dev skills** | [gamedev-skills/awesome-gamedev-agent-skills](https://github.com/gamedev-skills/awesome-gamedev-agent-skills) (Apache-2.0) | 77 playbooks for engines (Godot, Unity, Unreal, Bevy, Phaser, three.js and more), disciplines, genres and workflows (game jam, Steam and itch.io publishing). A router skill detects the engine and loads only the matching skills. | many skills, router | none |
| **REA** | [morluto/rea](https://github.com/morluto/rea) (MIT) | Reverse engineering for agents: native binaries, Electron/JS apps, .NET, Android APKs, websites, firmware, packet captures. Findings come with evidence and unknowns. | MCP server + investigation skill + CLI | Node 22+; optional Ghidra, Hopper, IDA, Rizin, objdump |

**Library:**
- Velocity keeps one library of packs on your machine. A pack is a git clone pinned to a commit.
- **Updates:** Velocity checks for new commits and shows the diff of `SKILL.md` files, scripts and data before moving the pin. A pack never updates silently, because a skill is instructions every agent follows.
- **Windows:** packs are cloned with `core.symlinks=true` and their Python runs with `PYTHONUTF8=1`. claude-skills needs both, or its mirror folders check out as pointer files.
- **Velocity's own installer:** Velocity installs packs itself instead of running each pack's installer (`npx skills add`, `uipro init`, `rea-agents setup`, Claude plugin marketplaces). One install then reaches all four providers, and nothing edits each agent's own config.

**Per project** (Settings → Skills, scoped to the current project):
- A switch per pack, plus bundle chips for packs that have bundles: claude-skills by domain, and Game dev skills by engine, discipline, genre and workflow.
- **Context budget:** a meter shows how many skills are on and roughly how many tokens of skill descriptions every agent loads at start. It turns amber above 150 skills. claude-skills defaults to Engineering only for this reason.
- **Suggestions** from the project scan (F3), switched on automatically:
  - UI UX Pro Max when a UI is found: web frameworks in `package.json`, Tailwind, SwiftUI, Flutter or Compose files.
  - Game dev skills (the router plus the detected engine's bundle) when an engine is found: `project.godot`, `*.uproject`, `ProjectSettings/`, or a Bevy, Phaser, PixiJS or three.js dependency.

  Velocity re-checks after every merge, so a project that gains a UI later gets the suggestion then.
- **Requirements:** each card shows what the pack needs and whether it's found, e.g. "✓ Python 3.12", "· Ghidra optional · not found".

**Where agents find them:**

| Provider | Folder |
|---|---|
| Claude Code | `.claude/skills/` |
| Codex, Antigravity | `.agents/skills/` |
| Kimi Code | `.kimi-code/skills/` (to confirm, Q26) |

- **Linked** (default): each enabled skill is a directory junction (Windows) or symlink into the library, created in every worktree and in the main checkout the spec interview uses. Those paths are listed in the repo's `.git/info/exclude`, so they never reach a commit or a PR.
- **Committed** (per project): the enabled skills are copied into the repo at the pinned commit, for collaborators who don't use Velocity. Updating the pin then goes through a PR like any other change.
- **Skills you installed yourself** are left alone (§3.2 Tool access). Velocity's links sit next to them. If a name clashes, your own skill wins and the card says so.

**Every role gets them:** threads, swarm and goal swarm members, PR duty, scouts, the main watcher and spec interviews all see the same enabled skills. Skills add knowledge; they never remove a tool.

**Pack specifics:**
- **UI UX Pro Max: design system.**
  - **Generate** runs its search with `--design-system --persist` and writes `design-system/<project>/MASTER.md`, plus page overrides. The design system is committed like SPEC.md.
  - While it exists, AGENTS.md gets the rule "Before any UI work, read `design-system/<project>/MASTER.md`. Use only its colours, type and spacing." (F11).
  - Scouts treat UI that breaks the design system as spec drift.
- **claude-skills: Python tools.** Its scripts run with your Python 3 and need no installs. Agents call them from the linked skill folder.
- **Game dev skills: router.** The router skill is always on with the pack. Engine bundles are exclusive (only the detected engine's bundle is suggested); disciplines, genres and workflows add on.
- **REA: MCP server.**
  - Turning REA on registers its MCP server in Velocity's shared MCP servers (§3.2), so every agent of every provider gets REA's tools with no per-agent setup. Its investigation skill is linked like any other.
  - Analysis runs locally, under your user, inside the agent's worktree.
  - The card says REA is for software you own or are authorized to examine, and that commercial engines (IDA, Hopper, JEB) need your own licences.

**Accept when:**
- [ ] Turning a pack on makes its skills available to the next agent of every signed-in provider, with no per-provider step.
- [ ] Linked skills never appear in `git status` or in a PR.
- [ ] A pack's pin moves only after its diff is shown, and only when you confirm.
- [ ] Turning REA on gives every new agent REA's MCP tools.
- [ ] The `$` menu lists exactly the project's enabled skills.
- [ ] Opening a Godot project suggests the router and the Godot bundle, and opening a Svelte app suggests UI UX Pro Max.

## 5. Keyboard map

| Key | Where | Action |
|---|---|---|
| `Ctrl+K` | anywhere | command palette |
| `Ctrl+N` | anywhere | new thread |
| `Ctrl+Shift+N` | anywhere | new project |
| `Ctrl+,` | anywhere | settings |
| `Alt` (hold) / `Alt+1–9` | anywhere | show thread numbers / jump to thread |
| `/` | not typing | search threads |
| `T` | not typing | focus "Add a task" |
| `W` | not typing | swarm dialog |
| `G` | not typing, outside a spec interview | goal swarm dialog |
| `A` | not typing | AIRC |
| `Esc` | anywhere | close overlay / menu |
| `Ctrl+Enter` | dialogs | create / start / launch |
| `I` `R` `F` `V` `S` | home view | dispatch, resume, fan out, PR duty, scouts |
| `1–4`, `L` `S` `G` | spec interview | answer; lock, SPEC.md, keep grilling |
| `D`, `E` | thread | diff, editor |
| `Ctrl+S` and the outliner keys | AGENTS.md | see F11 |

---

## 6. Visual language

- **Base:** OLED, Apple-inspired **graphite** with glassy panes. Black is true black.
  - **Window:** opaque `#000`. There is no desktop show-through (no Mica, Acrylic or vibrancy) because true black is the point.
  - **Layout:**
    - The sidebar is one floating glass pane, and each rail panel is its own pane.
    - The main column has no panel: content sits straight on black.
  - **Materials:**
    - Panes are smoked glass:
      - a top-lit fill `rgba(255,255,255,.072)` → `.034` over black, plus a faint top-left sheen;
      - a 1px specular rim, a 165° gradient from `rgba(255,255,255,.30)` at the top-left fading to `.04` and back to `.11` at the bottom-right;
      - a 1px inner top highlight.
    - The rim brightens on focus (the composer) and on hover (chips).
    - Popovers, menus, dialogs and toasts are frosted glass: `rgba(44,44,48,.72)` → `rgba(26,26,28,.80)` with `blur(48px) saturate(1.9)` over the content beneath them. The scrim dims to `rgba(0,0,0,.5)` and blurs.
    - Cards in the content area take a faint top-lit fill and a `rgba(255,255,255,.09)` hairline.
    - No drop shadows on panes, because they don't read on black.
  - **Shape:** corner radii 6 / 10 / 14 px. Panes 14–16 px, the composer and dialogs 18 px, menus 12 px. Pills and toasts are fully round.
  - **Controls:** the accent is graphite grey `#98989d`, used for selection, focus rings, checkboxes and the primary button.
    - Thread rows are source-list rows. The selection is a small top-lit glass lozenge, not an outlined box.
    - Segmented controls use a sliding raised segment, and buttons are subtle raised gradients.
    - The send button is a white filled circle, and shortcuts are keycaps.
  - **Type:** SF Pro / system UI, falling back to Inter (bundled), with SF Mono falling back to JetBrains Mono for code and meta.
    - There are no italics anywhere, including placeholders.
    - Hierarchy comes from weight and the label greys: titles `#fff`, primary `#f5f5f7`, secondary 62%, tertiary 36%.
- **State colours:**

  | Colour | Use |
  |---|---|
  | ok `#9ccc95` | merged / done |
  | warn `#e6b667` | CI / fixing |
  | err `#e8807a` | failed / changes requested |
  | info `#8fb4e8` | spec / PR open |
  | duty `#b7a3e6` | duty agents |
  | swarm `#7fd1c7` | swarm |
  | claude `#d97757` · codex `#e9e9e9` · agy `#8ab4f8` · kimi `#f08fb8` | provider marks (Anthropic, Codex, Antigravity, Kimi Code) |

- Base tokens are copied exactly from the mockup's `:root` into `ui/src/lib/theme/tokens.css`. Values missing from `:root` are copied from the reference's winning CSS declarations into `ui/src/lib/theme/reference.css` (user-authorized in #12). Components use the token files; the reference is read-only. The Rust theme module mirrors the base tokens.
- Fonts are bundled locally in the app. The mockup uses a CDN only for convenience.

---

## 7. Implementation order

Each milestone ends with a working app. PR duty checks an item off when it lands that item's PR (F7).

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
- [x] Spec interview: scan, ask, ready, lock; SPEC.md written and committed; draft → published tasks (F3)
- [x] Gate: no build work on unlocked projects (Rule 2)

**M3 · Threads and worktrees**
- [x] Agent runner: providers, models, effort, speed (Regular / Fast where the agent offers it), full permissions, queued steering (Rule 1, F4, F15)
- [x] Thread titles from the title model (default GPT-6 Luna · Low · Fast), in parallel with taking a worktree; branch named from the title (F4, F15)
- [x] ACP client (`agent-client-protocol`): spawn per thread, `session/new` in the worktree with AIRC as an MCP server, updates → transcript, auto-allow permissions, cancel, queued prompts, `fs`/`terminal` rooted at the worktree but not confined to it, full tool access with your config loaded and shared MCP servers (§3.2)
- [ ] ACP agents for Claude Code (`claude-code-acp`), Codex (`codex-acp`), Kimi Code (`kimi --acp`) and Antigravity, each using its own OAuth login through `authenticate`; Settings → Providers, model menu, limit meter (F15)
- [x] Worktree manager: create / delete / prune, path scheme, setup step (copy files, run command, shared build cache) (F5)
- [x] Thread view: transcript, tool steps, diffs, header with worktree path, composer, slash menu (F4)
- [x] Settings → Worktrees, including the live list and the footer count (F5, F13)

**M4 · PR loop**
- [x] GitHub integration: open PR, CI checks, reviews, comments, merge, close
  - [x] Live Open Pull Requests rail: real project PRs, CI, line counts, review state, reviewer claims and owning-thread navigation (F6/F7)
- [x] PR follow-through: track, PR history, auto-response to review comments, merge → worktree delete (F6)
- [x] Closed-without-merge state with the worktree kept (F5, F6)
- [x] Right-click menu and delete with 5 s undo (F12)

**M5 · Duty agents**
- [x] PR duty: claiming, review against SPEC.md, request changes, approve, merge; never merge unapproved; approval record bound to the head SHA; severity bar (Rule 4, F7, Q28)
- [x] Scouts: sweeps, issue filing with repro, de-duplication (F7)
  - [x] GitHub issue inventory (open and closed), executed failure evidence, duplicate assessment, dispatch labels and scout attribution; runnable backend filing path
- [x] Duty view, start dialog, pause/resume, add agents (F7)

**M6 · Task List, issues, swarm**
- [ ] Task List: states, add input, `T`, `/task`, palette, attribution, live agent state (F8)
- [ ] Agent `task_add` tool, used only when asked (F8)
- [x] Issues panel and single dispatch (F9)
- [ ] Swarm: dialog, launch, PR duty capacity, sidebar row, view, broadcast, + Items, rebase on merge, task done / issue close (F10)

**M7 · AGENTS.md**
- [x] Outliner editor with markdown round-trip (F11)
- [ ] Live lint, coverage checks with Add (F11)
- [x] Rewrite agent on Enter: tighten / expand / split / flag, with Undo (F11)
- [x] Default AGENTS.md template with the PR, Task List and AIRC rules (F11)

**M8 · AIRC**
- [x] Channel per project: log files, nicks, join/leave, MCP tools for agents (F14)
- [ ] Claims from each agent's first step; overlap detection and `velocity` lines (F14)
- [ ] Dependency wait → rebase → continue in the agent runner (F14)
- [x] Mention delivery to threads at step boundaries; your comments (F14)
- [x] Event log posted by Velocity from claims, GitHub and the Task List; agent messages kept separate (F14)
- [ ] Modal: Events (default), Claims, Messages, Discoveries, Past work, nick list, unread badge, `A` (F14)

**M9 · Goal swarm**
- [ ] Atomic claim lock: `airc_claim` / `airc_release`, refusal lines, held claims on failure (F14, F16)
- [x] AIRC MCP server injected into every provider's agent runtime (F14, F16)
- [ ] Work pool: open Task List items, open issues, unbuilt SPEC.md sections (F16)
- [ ] Dialog: prompt, count, model mix with "the rest", effort and speed per row, limits, pool size, PR duty capacity, keep going (F16)
- [ ] Launch, choosing → claimed → normal lifecycle, leave when nothing is left, keep going (F16)
- [ ] Goal swarm view: claims board, one card per agent, activity; sidebar row (F16)

**M10 · Throughput** (§3.3)
- [ ] Agent activity, stall detection with nudge then restart, per-agent caps, claim release after the grace period (§2.2)
- [ ] Parking: end the session on `airc_wait`, resume on the subscribed `merged` event; optional provider concurrency cap (§2.2, F15)
- [ ] Targeted AIRC delivery, message envelope (`to`, `intent`), heads-down and quarantine modes (F14)
- [ ] Targeted rebases, optimistic landing, conflicts sent back to the owning agent; rebuild the merge result whenever `master` moved; Shared files exempt from overlaps (F6, F14)
- [ ] Warm worktree pool from one base clone, prebuilt at the tip of `master` with a shared compiler cache; worktree guards (F5)
- [ ] PR duty tiered review and intent review; Sensitive paths in AGENTS.md (F7)
- [ ] Main watcher duty role: post-merge suite, bisect, fix or revert PR (F7)

**M11 · Proofs** (F17)
- [ ] Lean toolchain and Mathlib cache in the warm pool (F5)
- [ ] Proof checks in the worktree: `lake build`, no `sorry`/`admit`, axiom audit, statement lock and review reset
- [ ] Proof card on the PR, "Proved" on the PR track, `proved`/`statement`/`stress` events
- [ ] Merge gate in PR duty; statement review; tiers updated (F7)
- [ ] Scouts: assume-it-fails sweeps, evidence required, Stress and Proof gaps focus areas, stress runs against Lean reference models
- [ ] Levels per language: reference by default (fixture table plus one test; compiled Lean model through FFI for stress), code on request (Aeneas for Rust), model
- [ ] AGENTS.md: Implementation first, Proofs, and Tests and docs sections; Covers check (F11)

**M12 · Skills** (F18)
- [ ] Skill library: clone and pin packs, update with a diff, Windows symlink and UTF-8 handling
- [ ] Settings → Skills: per-project switches, bundles, context budget, requirements, add any Agent Skills repo
- [ ] Link enabled skills into every worktree and the spec checkout per provider folder; `.git/info/exclude`; committed mode
- [ ] Built-in packs: UI UX Pro Max (design system and AGENTS.md rule), claude-skills (bundles), Game dev skills (router and engine detection), REA (shared MCP server)
- [ ] Project scan suggestions (F3), re-checked after merges
- [ ] `$` skill menu in the composer; skill loads shown as tool steps (F4)

---

## 8. Open questions

| # | Question | Blocks |
|---|---|---|
| Q1 | What is the full meaning of the Task List? (The user will explain.) | anything beyond F8 |
| Q2 | How is "the implementation matches the spec exactly" verified? | a spec-check feature |
| Q3 | Final names for "duty", "PR duty" and "Scouts". | labels only |
| Q4 | Does a swarm ever cap how many agents run at once, or always start everything? Built: everything starts. A per-provider cap is specified, off by default (F15). | F10 |
| Q5 | Does a running swarm pick up items added after launch? (Built: no.) | F10 |
| Q6 | After the spec is locked, does a task added outside the spec update SPEC.md (via a short interview), or stay outside it? | F3, F8 |
| Q7 | Decided: no. The spec interview writes SPEC.md only; AGENTS.md comes from F11. | F3, F11 |
| Q8 | Does the AGENTS.md lint only advise, or also block (e.g. PR duty flags hedged wording in AGENTS.md changes)? | F11 |
| Q10 | Ghost-text autocomplete while typing in AGENTS.md? (It would take Tab from indent.) | F11 |
| Q11 | Decided: Svelte 5 + TypeScript on Vite, built with pnpm (§3). | M1 |
| Q12 | Should AIRC be one channel per project, or one shared channel across projects (as in xros_chat)? Built: per project. | F14 |
| Q13 | Should agents be able to schedule jobs for each other through AIRC (xros_chat job queues)? | F14 |
| Q14 | Which ACP agent does Antigravity use (an ACP mode of its own, or Gemini CLI's ACP mode on the same Google sign-in)? And how does each provider report its subscription limits, which ACP doesn't carry? | F15, §3.2 |
| Q15 | When a provider hits its subscription limit, should queued and new work move to another signed-in provider, or wait for the reset? | F15, F10, F16 |
| Q16 | May goal swarm agents claim work outside the Task List, issues and SPEC.md (split a large item, or claim something they found)? Built: no. | F16 |
| Q17 | When there are more agents than unclaimed pieces, should the extras leave (built), or pair up on existing pieces (tests, review)? | F16 |
| Q18 | Should "keep going" be on by default? Built: off, which matches the prompt you use today (stop after the merge). | F16 |
| Q19 | Should Velocity also accept any other ACP agent (a custom command and arguments, as Zed allows), or only the four providers? | F15, §3.2 |
| Q20 | Defaults for the stall timeout (5 min) and the claim-release grace period (10 min)? Should a failed agent's pushed branch be offered to whoever claims the piece next? | §2.2 |
| Q21 | Decided: pre-merge GitHub CI is build and lint and only advisory, the local checks on the merge result decide the merge (F7), proofs are checked locally before merge, and tests and stress scripts run after the merge (F17). | F7, F17 |
| Q22 | Name for the "Main watcher" duty role. | F7 |
| Q23 | Which code-level translators should ship besides Aeneas for Rust (C, C++, Python and others)? Until one exists for a language, that language tops out at the reference level. | F17 |
| Q24 | Decided: no. The spec interview only proposes the proof list; each building agent writes its design proof after the code (Rule 8). | F3, F17 |
| Q25 | Does fast mode draw down each provider's subscription limit faster, and how does each provider report it? | F15 |
| Q26 | Which folder does Kimi Code read skills from inside a project (`.kimi-code/skills/`, `.agents/skills/` or another)? And does Antigravity read `.agents/skills/` as the gamedev and UI UX Pro Max docs say? | F18 |
| Q27 | Should "Add a skill pack" also search the skills.sh registry, or take only GitHub repos? | F18 |
| Q28 | How is Rule 4 approval recorded when all agents share one GitHub account, which cannot approve its own PRs? Tracked in #42. Proposed: Velocity's own approval record bound to the head SHA (F7). Velocity's own repository uses a PR duty GitHub App until that is built (AGENTS.md). | M4 approve/merge, F7 |
