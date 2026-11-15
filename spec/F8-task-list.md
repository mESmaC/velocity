<!-- split-spec: {"order":1115,"kind":"markdown","indentation":0} -->
### F8 · Task List

**Continuous issue dispatch (Rosen projects):** Each project can save **Keep N agents busy** (off at 0) and a pause state.
Velocity fills free slots with ordinary build threads on the default model, using the issue title and the issue-dispatch prompt.
The work inventory excludes claimed issues, owner-decision questions (labelled `spec-question`, `needs-decision`, `needs-owner` or `owner-decision`, or explicitly requesting a decision in their title or body) and `release-blocker` issues.
Issues are ordered by milestone number (no milestone last), then oldest first.
Working and awaiting-land build threads count toward N; a land or an ended turn with no named work and an empty Rosen diff frees its slot.
Failed and closed threads free capacity but retain issue ownership.
An unchanged issue already dispatched is not dispatched again just because its agent found no changes or its GitHub issue has not closed yet; editing the issue makes it eligible again once unclaimed.
Dispatch continues across project switches and app restarts.
Interrupted startup recovery reserves its threads before background dispatch starts.
Empty-thread retirement excludes unread queued messages and rechecks saved state under the same lifecycle lease as resumption.
The sidebar's Duty controls and each Duty view show the slot and busy counts and Pause/Resume; the Duty view also shows the next five available issues.
Pausing stops new starts and leaves existing threads running.
The saved spec lock still applies.

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
- **Agents:** they have full create, read, update and delete over the Task List through `task_list`, `task_add`, `task_update` and `task_delete` (#309).
  They read it at any time, and add, change or remove tasks **only when your message asks them to** (e.g. "add these to the task list: X, Y", "mark 3 done", "drop task 5").
  Otherwise they file issues.
  This rule is written in AGENTS.md.
- **Agent tool runtime:** the Task List tools are served by the AIRC MCP server injected into every agent session.
  Its instructions say to change the list only for an explicit user request; otherwise file issues.
  Only the native store owner writes `state.json`:
  - `task_list` returns each task's number, title, state, owning thread with its live state, and who added it.
    It reads a snapshot the store owner publishes in the project's channel on every pass (about once a second), and before it answers any change, so a change the agent just made is always in it.
  - `task_add { title }`, `task_update { task, title?, state? }` (state `todo` or `done`) and `task_delete { task }` queue a durable project-scoped request under the launcher's identity.
    The store owner applies it, saves the result with thread attribution (duty agents link to their role) and posts a `task` event.
    The tool waits up to 5 s for the answer and returns it: the saved task, or why it was refused.
    Past that it returns the queued request sequence, and `read_feed` carries the answer.
    Request identities prevent duplicate adds after a crash/restart.
  - An added task is a draft before lock and todo afterward.
    Every task stays a draft until the spec is locked, so a state change needs a locked spec.
    A task an agent is on (now) can be marked done but not reopened.
    Reopening a done task frees it from the thread that finished it, so that thread's merge cannot close it again.
    Deleting a task drops it from any thread that lists it; the thread keeps running.
  - None of them edits SPEC.md or starts an interview.
- Tasks not created by the spec show who added them: "added by you", or "added by <thread>", which links to that thread.
- Tasks added before the spec is locked are drafts.
- **After the spec is locked** (decided, Q6): a task you or an agent adds goes straight to todo and stays outside SPEC.md.
  Adding a task never starts an interview and never edits SPEC.md.
  When the work changes behaviour SPEC.md describes, the building agent updates SPEC.md in its own PR (AGENTS.md).

**Live state:** a task with an agent shows that agent's state (agent working / agent idle / #n · in review / merged · #n), linked to its thread.
Agent idle means its turn ended before its PR is followed, such as a draft claim not yet marked ready.

**Accept when:**
- [ ] Re-rendering the list never takes focus from the add-task input.
- [ ] New tasks scroll into view.

> The full meaning of the Task List is still to be explained by the user (§8).
Do not add task fields or behaviour beyond this section.
