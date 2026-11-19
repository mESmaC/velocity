<!-- split-spec: {"order":1160,"kind":"markdown","indentation":0} -->
### F10 · Swarm

One agent per open item, all started at the same time, each in its own workspace.
Open it with **Swarm** in Work on Rosen projects, with **Swarm** on either Task List or Issues on git projects, or with `W`, the home chip, or the palette.
The Work entry opens the dialog with both sources selected.

**Swarm dialog:**
- **Source:** Task List · N, Issues · N, or Both · N.
- **Items:** checkboxes, all open items checked by default, with Select all/none.
  Items that already have an agent, and draft tasks, are shown but can't be selected.
- **Models:** choose one or several.
  Several are assigned round-robin, and the split is shown ("3 on Opus · 3 on Sonnet · 3 on Codex").
  Launches interleave groups from the code graph's likely subsystems (F19), and each classified item gets its subsystem as a starting hint.
- **Rules:**
  - Each agent names its change, and Rosen lands it once it combines cleanly with the base (locked, from AGENTS.md).
  - Each agent gets its own workspace under the configured location (locked).
  - Rosen rebases each agent onto the base at its next step boundary after a change lands (locked, F5); Velocity runs no git rebase for swarm members.
  - Close each issue once its change lands (default on).
  The dialog sizes no reviewer pool: after-land review (F7) covers landed changes without holding them.
- **Footer:** "N workspaces on D: · all start now".
  Launch with `Ctrl+Enter`.

**Launch:** a launch raises the Rosen warm pool on the machines it uses to the member count (Rosen §9), so members take ready workspaces instead of preparing their own.

**Swarm in the sidebar:** one row under **Swarms**, with ×N, a segmented progress bar (editing / named / reopened / landed) and counts.
Member threads are listed under it, indented, only while the swarm or one of its members is selected.
They are not listed in the other groups.
×N and the counts follow live members only (editing, named, reopened): landed, closed, failed and deleted members drop out, a member being deleted drops out at once, and a swarm with no live member left reads "finished".
Right-clicking the row deletes the swarm (F12).

**Lifecycle:** each member is editing → named → landed, and a refused land or a post-land revert reopens its change for the same member (reverted → reopened) until it lands again.

**Swarm view:**
- **Header:** N agents · N workspaces live · start time · landed/total.
  **+ Items** adds open items to this swarm.
- A big progress bar plus a legend.
- **Agent cards:**
  - the item (task, or issue #n)
  - a 3-dot track: editing · named · landed
  - what it is doing now ("named · <change>", "reopened · <change>", "landed · <change>")
  - agent id, model and +/−

  Click a card to open that agent's thread.
- **Activity:** named, reopened after a refused land, reverted · reopened, landed (task done or issue #n), abandoned, stalls, and "Swarm finished · all N changes landed".
  Rosen change events write these lines in the same save as the member's own progress.
- **Composer:** a message goes to every agent still working; each picks it up at its next step.

**Landing a member** ticks its task done or completes its issue, removes its change from the rail and removes its workspace.
Legacy git projects keep their PR follow-through (F6) until they migrate to Rosen; their cards show the change's checks and review.

**Accept when:**
- [ ] All selected agents start immediately, unless a provider concurrency cap is set (F15, off by default; §8 Q4).
- [ ] Deleting a member returns its task to todo, or frees its issue.
- [ ] A swarm on a Rosen project shows no PR, merge or rebase state in its dialog, card tracks, legend or activity.
