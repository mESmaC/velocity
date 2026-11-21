<!-- split-spec: {"order":298,"kind":"markdown","indentation":0} -->
### F1 · Main window

There are two views of the same work, switched from the title bar or with `B`:
- **Focus:** one thread in depth, in three columns:
  - **left:** sidebar, 236 px
  - **middle:** the main view
  - **right:** a rail, 236 px
- **Board:** every thread at once, as cards in lanes, in place of all three columns (see Board below).

**Title bar:**
- **Brand and project switcher:** a dropdown listing every project.
  Each entry shows whether it is local or on GitHub, or "speccing" if its spec isn't locked yet.
  - Switching restores that project's saved active thread, sidebar and rail context, clears transient search and input, and closes the dropdown.
    The last active project and each project's active thread survive restart.
    Unknown project IDs leave the current selection unchanged.
  - Below the list: New project… (F2), Open folder… and Open from GitHub… (F2 Open project).
- **View switch:** a Focus | Board segmented control, shown once a project is open.
- **Pulse:** live counts, each shown only when it is non-zero:
  - on duty
  - in spec
  - running
  - changes watched
  - failed
  - landed today: changes that landed since local midnight, counted as each land arrives and kept across a restart
- **Local build** (#512, #551): Velocity running its own landed fixes, shown only where a Rosen project's GitHub repository is Velocity's own (`mESmaC/velocity`).
  - Once a minute Velocity reads its last green integration commit—catching up from a GitHub mirror sequentially before each check, and from a Collapse mirror right before starting a build so idle checks never wake Collapse (#572).
    Every release build (`release.yml`, `scripts/release-local.ps1` and local builds) embeds its commit in build metadata (`VELOCITY_BUILD_COMMIT`).
    Velocity builds and offers a commit only when it strictly descends from the running build's commit in Rosen's history.
    When that commit is not built yet and has not failed, Velocity stages it in the project's `local-release` folder, beside its workspaces, and runs `pnpm -C ui install --frozen-lockfile` and `scripts/release-local.ps1` there.
    The build takes one background turn in the machine's run queue, at low process priority behind landing checks and agents' own builds.
    It starts only when 4 GB of memory is free beyond the usual reserve; until then the title bar says it is waiting for memory.
  - A build's version sorts above the release the running copy was built on and below the next one: `0.1.0-beta.13.local.g<commit>` on `0.1.0-beta.13`, and `0.2.1-local.g<commit>` on `0.2.0`.
    The update check therefore still offers the next published release, and never the one a local build already contains.
  - The newest two builds are kept in `local-builds` in the app data folder, one folder per commit, with the executable, the setup and a record.
  - **Restart into `<commit>`** shows only when the newest build strictly descends from the running copy.
    Installing never replaces Velocity with an ancestor of the running build; if the running build's commit or ancestry is unknown, Velocity asks first and names both commits.
    **Restart into previous** shows while the running copy is the newest build and the build before it is kept, as an explicit rollback.
    Either one saves state, stops the Rosen servers, replaces the running executable with the build's and starts it, as a portable update does, so stopped turns continue by themselves (§3.2 Resume after a restart).
    Both wait while a build is running.
  - A failed build shows "Build of `<commit>` failed" with the reason in its tooltip.
    That commit is not built again; the next green commit is.
  - After a published update, builds made on the older release are no longer offered.
  - Development builds neither build nor restart.
    Local builds are never published or signed: releases stay on CI, which alone holds the updater key.
- A `Ctrl K` button that opens the palette.
- The window controls.

**Window recovery**

- While the layout loads, the window shows its controls and “Reconnecting…”.
  If startup fails or times out, the window shows the error and a Reload button.
- If a visible window misses its renderer heartbeat for five seconds, Rust logs the timeout in `velocity.log` and reloads the webview in place.
  The layout and agent sessions remain in Rust.
- A layout dispatch that exceeds ten seconds logs a timeout and releases the next request in the queue.

**Board:**
- Lanes read left to right as a pipeline.
  Each has a title, a count and its cards:
  - **Up next:** tasks nobody has started (todo or draft), then open issues.
    It is drawn as a dashed outline, because this work isn't in the pipeline yet, and it has a Swarm button when it holds anything.
  - **Running:** swarms, spec interviews, working threads and failed threads.
    A failed card is tinted red.
  - **In review:** threads whose PR is open.
  - **Finished:** threads that merged, are done, or abandoned.
  - **Review:** Scouts and the review swarm.
    They are not work items.
- The four pipeline lanes always show, with a one-line note when empty, so the pipeline keeps its shape.
- A thread card shows the thread's state icon and colour, its title and the same meta line as its sidebar row.
  The selected thread's card has the accent selection.
- Clicking a thread card opens that thread and returns to Focus.
  Task and issue cards are not clickable yet.
- The lanes are computed in Rust from the sidebar groups, the Task List and the issue list, so the Board never disagrees with the Focus view.

**Sidebar:**
- search (`/`) and a new-thread button
- the thread groups (§2.1)
- a footer with settings, usage, updates, a live workspace count ("8 on D:", click → Settings · Workspaces) and a subscription-limit meter: one bar per signed-in provider, filled to the share of its limit used, coloured by provider.
  The usage button and the meter both open the usage modal (F15 Usage modal).
  There is no dollar spend: every provider runs on a subscription.
  - **Updates:** Velocity checks its GitHub Releases for a newer version 30 seconds after it starts, every 6 hours, and when you press Check for updates.
    While one is available, the Updates button turns accent-coloured.
    It opens the Updates modal, a centred dialog like the usage modal (Esc or a click outside closes it), which shows the new version, its release notes, how this copy was installed (setup, MSI or portable), how many agents are mid-turn, and **Restart to update**.
    Restarting stops running turns; workspaces and transcripts are kept, and each stopped turn continues by itself when Velocity starts again (§3.2 Resume after a restart).
    Every download is verified against Velocity's release signing key, with the version bound into the signature, before anything runs.
    Setup and MSI copies run their installer, which restarts Velocity.
    A portable copy replaces its own executable and starts again.
    Installing an update restarts Velocity only when you press the button; fatal process exits recover automatically (§3.2).
    There is one channel: the newest release, betas included.
    A `v*` tag publishes a release (`.github/workflows/release.yml`); development builds check only when asked and never install.
    A local build (`scripts/release-local.ps1`) never signs downloads or moves `updater/latest.json`, uploads only as a draft labelled as an unsigned local build (`--draft --prerelease --latest=false`), and clearly states that the updater manifest was not moved (#521).
    Any published `v*` release missing signed updater assets (`latest.json` and `.sig`) is marked `--latest=false` and labelled as not offered to installed copies, and `release.yml` reports when `updater/latest.json` was not moved (#521).
    Each update check also compares `updater/latest.json` against the newest published `v*` release and reports in the Updates modal and sidebar trigger when the manifest is behind (#521).
    A mirror forwarder delivers only the default branch, so Cut release in the Rosen status pane (F6) dispatches that workflow with the tag and landed commit, and the workflow creates the tag when it publishes (#431).
    The release build runs beside the release gate, and publishing waits for both (#498).
    A `-beta` release whose exact commit Rosen reports green, with no open release-blocker issue, skips the gate's stress suite and proofs; numbered releases always run the full gate (#434).
    The default branch's build keeps the pinned Rosen built in a cache every release restores, so the release build does not wait for a Rosen source build (#434).
    Each release writes every job's time and the time from trigger to published to its run summary, and a `-beta` release that took over 20 minutes gets a warning (#434).
    A failed check or download shows in the modal, not as a window error.

**Rail:** Rosen projects show Work (F8, F9) and Rosen status (F6).
Git projects show Task List and Open Issues.
Below them are footer links: AIRC (F14), with an unread badge that turns amber when an overlap is unread, and AGENTS.md.
Each is a text link with the reference's ↖ mark, not a filled button.

**Home (empty) view:** a composer and quick chips:

| Chip | Key |
|---|---|
| Dispatch an issue | `I` |
| Resume last thread | `R` |
| Fan out ×3 | `F` |
| Start review agents | `S` |
| Swarm the Task List | `W` |

**Command palette** (`Ctrl+K`): fuzzy search over commands, projects, threads and issues.
Commands:
- new thread
- new project
- edit AGENTS.md
- add task
- settings
- dispatch issue
- fan out
- scouts
- swarm the Task List
- swarm the issues

**Accept when:**
- [ ] Window is frameless and can be dragged by the title bar.
      The custom window controls work.
- [ ] Pulse updates live from events and hides zero counts.
- [ ] Holding `Alt` shows numbers on threads, and `Alt+1`–`9` jumps to that thread.
- [ ] Every keyboard shortcut in §5 works.
