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
  - landed today
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
  - **On duty:** duty agents, in a last lane shown only when any exist.
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
    A failed check or download shows in the modal, not as a window error.

**Rail:** Task List, Rosen status (F6) for Rosen projects, and Open Issues.
Below them are footer links: AIRC (F14), with an unread badge that turns amber when an overlap is unread, and AGENTS.md.

**Home (empty) view:** a composer and quick chips:

| Chip | Key |
|---|---|
| Dispatch an issue | `I` |
| Resume last thread | `R` |
| Fan out ×3 | `F` |
| Put an agent on PR duty | `V` |
| Send scouts | `S` |
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
- PR duty
- scouts
- swarm the Task List
- swarm the issues

**Accept when:**
- [ ] Window is frameless and can be dragged by the title bar.
      The custom window controls work.
- [ ] Pulse updates live from events and hides zero counts.
- [ ] Holding `Alt` shows numbers on threads, and `Alt+1`–`9` jumps to that thread.
- [ ] Every keyboard shortcut in §5 works.
