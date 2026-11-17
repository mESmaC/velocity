<!-- split-spec: {"order":1688,"kind":"markdown","indentation":0} -->
### F18 · Skills

**What it is:** first-class support for skill packs: installed once, kept on a pinned commit, switched on per project, and given to every agent of every provider.
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
- **Updates:** Velocity checks for new commits and shows the diff of `SKILL.md` files, scripts and data before moving the pin.
  A pack never updates silently, because a skill is instructions every agent follows.
- **Windows:** packs are cloned with `core.symlinks=true` and their Python runs with `PYTHONUTF8=1`.
  claude-skills needs both, or its mirror folders check out as pointer files.
- **Velocity's own installer:** Velocity installs packs itself instead of running each pack's installer (`npx skills add`, `uipro init`, `rea-agents setup`, Claude plugin marketplaces).
  One install then reaches all four providers, and nothing edits each agent's own config.

**Per project** (Settings → Skills, scoped to the current project):
- A switch per pack, plus bundle chips for packs that have bundles: claude-skills by domain, and Game dev skills by engine, discipline, genre and workflow.
- **Context budget:** a meter shows how many skills are on and roughly how many tokens of skill descriptions every agent loads at start.
  It turns amber above 150 skills.
  claude-skills defaults to Engineering only for this reason.
- **Suggestions** from the project scan (F3), switched on automatically:
  - UI UX Pro Max when a UI is found: web frameworks in `package.json`, Tailwind, SwiftUI, Flutter or Compose files.
  - Game dev skills (the router plus the detected engine's bundle) when an engine is found: `project.godot`, `*.uproject`, `ProjectSettings/`, or a Bevy, Phaser, PixiJS or three.js dependency.

  Velocity re-checks after every land, so a project that gains a UI later gets the suggestion then.
  A suggested pack that is not installed says **Install to turn on**.
  Its switch can decline the suggestion before installation; that choice survives later scans and installation.
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

- **Linked** (default): each enabled skill is a directory junction (Windows) or symlink into the library, created in every workspace and in the main checkout the spec interview uses.
  Those paths are listed in the repo's `.git/info/exclude`, so they never reach a commit or a PR.
  - **Rosen workspaces (#272):** links are made after the take, before the setup command, alongside existing skills without replacing them. Each provider's skills folder gets exact local `.gitignore` rules for Velocity's links; a newly created ignore file excludes itself too. Existing ignore files are preserved and supplemented. These rules survive a rebase without putting the links in a snapshot. Pack MCP discovery identifies Rosen workspaces through the project's saved workspace root or thread path, so enabled servers reach Rosen agents without a git checkout.
- **Committed** (per project): the enabled skills are copied into the repo at the pinned commit, for collaborators who don't use Velocity.
  Updating the pin then goes through a PR like any other change.
- **Skills you installed yourself** are left alone (§3.2 Tool access).
  Velocity's links sit next to them.
  If a name clashes, your own skill wins and the card says so.

**Every role gets them:** threads, swarm and goal swarm members, PR duty, scouts, the main watcher and spec interviews all see the same enabled skills.
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
  - Turning REA on registers its MCP server in Velocity's shared MCP servers (§3.2), so every agent of every provider gets REA's tools with no per-agent setup.
    Its investigation skill is linked like any other.
  - Analysis runs locally, under your user, inside the agent's workspace.
  - The card says REA is for software you own or are authorized to examine, and that commercial engines (IDA, Hopper, JEB) need your own licences.

**Accept when:**
- [ ] Turning a pack on makes its skills available to the next agent of every signed-in provider, with no per-provider step.
- [ ] Linked skills never appear in `git status` or in a PR.
- [ ] A pack's pin moves only after its diff is shown, and only when you confirm.
- [ ] Turning REA on gives every new agent REA's MCP tools.
- [ ] The `$` menu lists exactly the project's enabled skills.
- [ ] Opening a Godot project suggests the router and the Godot bundle, and opening a Svelte app suggests UI UX Pro Max.
