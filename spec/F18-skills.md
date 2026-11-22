<!-- split-spec: {"order":1688,"kind":"markdown","indentation":0} -->
### F18 · Skills

**What it is:** first-class support for skill packs: installed once, kept on a pinned commit, available on demand from the composer in every project.
Four packs are built in.
Any other repo that follows the Agent Skills standard can be added.

| Pack | Repo | What it is | Shape | Needs |
|---|---|---|---|---|
| **UI UX Pro Max** | [nextlevelbuilder/ui-ux-pro-max-skill](https://github.com/nextlevelbuilder/ui-ux-pro-max-skill) (MIT) | Design intelligence: 80 styles, 192 palettes, 74 font pairings, 25 chart types, 120 UX rules, guidelines for 22 stacks, behind a local BM25 search (`scripts/search.py`). | 1 skill, CSV data, stdlib Python | Python 3 |
| **claude-skills** | [alirezarezvani/claude-skills](https://github.com/alirezarezvani/claude-skills) (MIT) | 410 skills in domain bundles (engineering, product, marketing, research, regulatory, C-level, finance and more), with 807 stdlib-only Python tools and reference docs. | many skills, in bundles | Python 3 |
| **Game dev skills** | [gamedev-skills/awesome-gamedev-agent-skills](https://github.com/gamedev-skills/awesome-gamedev-agent-skills) (Apache-2.0) | 77 playbooks for engines (Godot, Unity, Unreal, Bevy, Phaser, three.js and more), disciplines, genres and workflows (game jam, Steam and itch.io publishing). A router skill detects the engine and loads only the matching skills. | many skills, router | none |
| **REA** | [morluto/rea](https://github.com/morluto/rea) (MIT) | Reverse engineering for agents: native binaries, Electron/JS apps, .NET, Android APKs, websites, firmware, packet captures. Findings come with evidence and unknowns. | MCP server + investigation skill + CLI | Node 22+; optional Ghidra, Hopper, IDA, Rizin, objdump |

**Library:**
- Velocity keeps one library of packs on your machine.
  A pack is a git clone pinned to a commit.
- **Settings → Skills** shows installed and available packs, with install, remove and update actions.
  Built-in and custom packs use the same library.
  Installed skills appear in the composer's `$` menu independently of bundle defaults.
  A duplicate name can be invoked using its pack-qualified name.
- **Default bundles** set the starting selection when a user first marks a pack always on in a project.
  claude-skills defaults to Engineering and Game dev defaults to its router.
- **Context budget** estimates skill counts and description tokens for the machine defaults and the current project's always-on selection.
  The meter turns amber above 150 skills.
- Installing a pack runs in the background and shows cloning, checkout, pinning and discovery on its Settings card.
  A failed install shows a retry action, and Velocity records the install steps and failures in `velocity.log`.
  Retrying after an interruption reuses a usable clone or replaces incomplete staging data.
  Agent launches continue during installation and see the new pack only after Velocity atomically saves its registry entry.
  A fresh install at the remote tip uses a shallow clone.
- **Updates:** Velocity checks for new commits and shows the diff of `SKILL.md` files, scripts and data before moving the pin.
  A pack never updates silently, because a skill is instructions every agent follows.
- **Windows:** packs are cloned with `core.symlinks=true` and their Python runs with `PYTHONUTF8=1`.
  claude-skills needs both, or its mirror folders check out as pointer files.
- **Velocity's own installer:** Velocity installs packs itself instead of running each pack's installer (`npx skills add`, `uipro init`, `rea-agents setup`, Claude plugin marketplaces).
  One install then reaches all four providers, and nothing edits each agent's own config.

**Always on for this project** (optional):
- New projects start with an empty always-on list.
  A project can select a pack and customize the bundles it loads at session start.
  Existing per-project pack enables and bundle selections become its always-on list.
  Installed packs and their pins stay in the machine-wide library.
- **Suggestions** from the project scan (F3), switched on automatically:
  - UI UX Pro Max when a UI is found: web frameworks in `package.json`, Tailwind, SwiftUI, Flutter or Compose files.
  - Game dev skills (the router plus the detected engine's bundle) when an engine is found: `project.godot`, `*.uproject`, `ProjectSettings/`, or a Bevy, Phaser, PixiJS or three.js dependency.

  Velocity re-checks after every land, so a project that gains a UI later gets the suggestion then.
  A suggested pack that is not installed says **Install for always on**.
  Its always-on switch can decline the suggestion before installation; that choice survives later scans and installation.
  Installing a selected pack activates it automatically.
  Turning a declined pack back on requires installation.
- **Requirements:** each card shows what the pack needs and whether it's found, e.g. "✓ Python 3.12", "· Ghidra optional · not found".
  Detection is cached for the app session, so switches and bundle clicks reuse the results; restart Velocity to detect newly installed tools.
  Optional tools are found by their launcher names on PATH, including Windows PATHEXT launchers such as `ghidraRun.bat` and `idat64.exe`/`ida64.exe`.

**Where agents find them:**

| Provider | Folder |
|---|---|
| Claude Code | `.claude/skills/` |
| Codex, Antigravity | `.agents/skills/` |
| Kimi Code | both `.kimi-code/skills/` and `.agents/skills/` (unverified, Q26) |

The provider-to-folder mapping is a data table in `velocity-skills`, not code, so a later correction changes one row.
Kimi Code's folder is unverified, so Velocity links into both folders: the links are junctions excluded from version control, so the second folder costs nothing and never reaches a commit.
When Kimi's folder is confirmed, the extra row is dropped, and Velocity removes its own links and exclude lines from a folder no provider uses any more.

- **Linked** (default): the session's always-on skills are directory junctions (Windows) or symlinks to the pinned library in the workspace or spec checkout.
  When a prompt invokes a skill, Velocity links that skill into the workspace and tells the agent which `SKILL.md` to read for the turn.
  The link stays available for the workspace's lifetime under `.velocity/invoked-skills`, where a later session in that checkout does not auto-load it.
  Git workspaces exclude links through `.git/info/exclude`.
  - **Rosen workspaces (#272):** always-on links are made after the take, before the setup command.
    Each provider folder gets exact local `.gitignore` rules, and a new ignore file excludes itself.
    Existing skills and ignore rules are preserved.
    Pack MCP discovery uses the registered project or workspace path.
- **Committed** (per project): the always-on skills are copied into the repo at the pinned commit, for collaborators who don't use Velocity.
  Updating the pin then goes through a change like any other change.
- **Skills you installed yourself** are left alone (§3.2 Tool access).
  Velocity's links sit next to them.
  If a name clashes, your own skill wins and the card says so.

**Every role gets them:** threads, swarm and goal swarm members, Scouts, review swarm workers and spec interviews all see the always-on selection for their project.
Skills add knowledge; they never remove a tool.

**Pack specifics:**
- **UI UX Pro Max: design system.**
  - **Generate** runs its search with `--design-system --persist` and writes `design-system/<project>/MASTER.md`, plus page overrides.
    The design system is committed like SPEC.md.
  - While it exists, AGENTS.md gets the rule "Before any UI work, read `design-system/<project>/MASTER.md`.
    Use only its colours, type and spacing."
    (F11).
  - Scouts treat UI that breaks the design system as spec drift.
- **claude-skills: Python tools.**
  Its scripts run with your Python 3 and need no installs.
  Agents call them from the linked skill folder.
- **Game dev skills: router.**
  The router skill is always on with the pack.
  Engine bundles are exclusive (only the detected engine's bundle is suggested); disciplines, genres and workflows add on.
- **REA: MCP server.**
  - Adding REA to a project's always-on selection registers its MCP server for new sessions in that project (§3.2).
    Its investigation skill is linked like any other.
  - Analysis runs locally, under your user, inside the agent's workspace.
  - The card says REA is for software you own or are authorized to examine, and that commercial engines (IDA, Hopper, JEB) need your own licences.

**Accept when:**
- [ ] Installing a pack once makes it available from the composer in any project.
- [ ] Linked skills never appear in a Rosen snapshot or change.
- [ ] A pack's pin moves only after its diff is shown, and only when you confirm.
- [ ] Adding REA to a project's always-on list gives its new sessions REA's MCP tools.
- [ ] The `$` menu lists installed skills and selecting one makes its manifest available to that turn.
- [ ] Opening a Godot project suggests the router and the Godot bundle, and opening a Svelte app suggests UI UX Pro Max.
