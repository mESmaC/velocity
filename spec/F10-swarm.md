<!-- split-spec: {"order":1160,"kind":"markdown","indentation":0} -->
### F10 · Swarm

One agent per open item, all started at the same time, each in its own workspace.
Open it with the **Swarm** buttons on the Task List and Issues panels, `W`, the home chip, or the palette.

**Swarm dialog:**
- **Source:** Task List · N, Issues · N, or Both · N.
- **Items:** checkboxes, all open items checked by default, with Select all/none.
  Items that already have an agent, and draft tasks, are shown but can't be selected.
- **Models:** choose one or several.
  Several are assigned round-robin, and the split is shown ("3 on Opus · 3 on Sonnet · 3 on Codex").
- **Rules:**
  - Each agent opens a PR and watches it until it lands (locked, from AGENTS.md).
  - Each agent gets its own workspace under the configured location (locked).
  - Rebase only when a merged PR touches your files, otherwise once at landing (locked, F6).
  - Close each issue when its PR merges (default on).
- **PR duty capacity:** aim for about 1 reviewer per 3 open PRs.
  If PR duty is short, offer "Add N agents to PR duty"; if nobody is on PR duty, offer to start it.
  Without a reviewer, changes wait as "needs a reviewer".
- **Footer:** "N workspaces on D: · all start now".
  Launch with `Ctrl+Enter`.

**Swarm in the sidebar:** one row under **Swarms**, with ×N, a segmented progress bar (working / in PR / fixing / merged) and counts.
Member threads are listed under it, indented, only while the swarm or one of its members is selected.
They are not listed in the other groups.
×N and the counts follow live members only (choosing, working, named, addressing review): merged, closed, failed and deleted members drop out, a member being deleted drops out at once, and a swarm with no live member left reads "finished".
Right-clicking the row deletes the swarm (F12).

**Swarm view:**
- **Header:** N agents · N workspaces live · start time · merged/total.
  **+ Items** adds open items to this swarm.
- A big progress bar plus a legend.
- **Agent cards:**
  - the item (task, or issue #n)
  - a 5-dot track: work · PR · CI · review · merged
  - what it is doing now
  - agent id, model and +/−

  Click a card to open that agent's thread.
- **Activity:** PR opened, changes requested, comment resolved, merged (task done / issue closed), "N affected agents rebased after #n", conflicts, stalls, and "Swarm finished · all N changes merged".
- **Composer:** a message goes to every agent still working; each picks it up at its next step.

**Merging a member** ticks its task done or closes its issue, removes the PR from the rail, deletes the workspace, and rebases only the members whose files it touched (F6).

**Accept when:**
- [ ] All selected agents start immediately, unless a provider concurrency cap is set (F15, off by default; §8 Q4).
- [ ] Deleting a member returns its task to todo, or frees its issue.
