<!-- split-spec: {"order":1115,"kind":"markdown","indentation":0} -->
### F8 · Task List

**Rosen Work panel (decided, #440).** The project has one Work panel for local Task List entries and GitHub issues (F9).
Each row has a **Task** or **GitHub issue** badge and keeps its source identity: project and task number, or repository and issue number.
Tasks stay in the local store and issues keep their discussion on GitHub.
Work and dispatch read the same F16 work inventory; unbuilt spec sections remain a Goal Swarm source.
Draft tasks stay out of dispatch until the spec is locked.

**Rosen work lifecycle.** The thread holds the claim and the Rosen change records the work items it addresses, including task IDs and repository-qualified issue IDs (F5, Issues carried by Rosen changes).
The event follower projects the item state from those associations, reusing the saved thread link for navigation.
Each row links its owning or completing thread and its change as soon as those links exist.

| State | When it appears |
|---|---|
| Todo | The item has no active owner or completion; dispatch applies its usual eligibility rules. |
| Claimed | A thread owns the item and has yet to name its change. |
| In change | The owning thread named its change; the row shows the change ID. |
| Landed | Rosen landed the change; the dispatch slot is free and the item stays out of the available pool. |
| Green | A successful post-land build covers the landed commit (F5, F7). |
| Reverted → Reopened | Rosen reverted the land and reopened the change. The item returns to active work with the revert reason and its original owner, which can name and land the change again. |

Abandoning an item's current change releases its claim and returns it to the source's available work, subject to the existing eligibility rules.
A stack handoff keeps the claim with the replacement change and late events from a superseded change leave that claim intact.
The follower saves the projection and its replay position together so restart, reconnect and event replay preserve state and ownership.
Rosen lifecycle updates reach the panel and the pool from the event follower without polling for change state.
GitHub's issue state and the full-test result remain separate from the Work status.

**Done history.** Green items move into collapsed **Done** groups, one per local calendar day of completion, newest day first.
An issue can be Green while its GitHub discussion remains open (F9).
Manually completed tasks join the group for their completion day; saved completions without a recoverable date appear under **Done · Date unavailable**.
Reopening a completed item removes it from its Done group and restores it to active work.
The Task List counter includes completed tasks in collapsed groups.

**Rollout.** First add the Rosen lifecycle behind the live work pool while the existing panels keep working ([#474](https://github.com/mESmaC/velocity/issues/474)).
Then replace both Rosen panels with Work in one change ([#475](https://github.com/mESmaC/velocity/issues/475)).
Continuous dispatch keeps its inventory, claim protection and slot refill throughout the rollout.
Local tasks participate in continuous dispatch during the rollout (#439).
Git projects keep the existing Task List and Issues panels and their PR lifecycle while git project mode is supported.

**Continuous dispatch (Rosen projects):** Each project can save **Keep N agents busy** (off at 0) and a pause state.
Velocity fills free slots with ordinary build threads on the default model, using the selected item's title and work prompt.
The queue takes user-added tasks first, then issues by milestone number (no milestone last) and age, then agent-added and spec-generated tasks.
Within each task group, the oldest task number goes first.
The work inventory excludes claimed items, owner-decision questions, release blockers and issues labelled `no-change`.
Continuous dispatch also skips GitHub issues with an open GitHub blocker and makes them eligible on the first issue refresh after their last blocker closes (#472).
Issues can carry the labels `spec-question`, `needs-decision`, `needs-owner`, `owner-decision` or `release-blocker`, and dispatch also checks issue and task text for explicit owner-decision wording and task titles for the `release-blocker` marker.
Working and awaiting-land build threads count toward N; a land or an ended turn with no named work and an empty Rosen diff frees its slot.
A dispatch worker takes its next item in a fresh ACP session after cleanup, with a bounded handoff from Velocity and a session boundary in the existing conversation (#456).
Failed and closed threads free capacity but retain issue ownership.
An unchanged issue already dispatched stays out of dispatch while its GitHub issue has not closed after a land.
When continuous dispatch retires an issue thread after an empty turn, it saves **Ended with no change** and the last agent message in the thread view (#469).
Dispatch holds the issue while the GitHub report is pending and then skips it while the `no-change` label is present (F9).
Once a person or review agent removes the label, an open, unclaimed issue can be dispatched again.
A dispatched task moves to now when its thread is saved and to done when that thread's change lands.
An empty turn returns its task to todo and continuous dispatch skips it until the task is edited or explicitly returned to todo.
Dispatch continues across project switches and app restarts.
Interrupted startup recovery reserves its threads before background dispatch starts.
Empty-thread retirement excludes unread queued messages and rechecks saved state under the same lifecycle lease as resumption.
The sidebar's Duty controls and each Duty view show the slot and busy counts and Pause/Resume; the Duty view also shows the next five available items in dispatch order.
Pausing stops new starts and leaves existing threads running.
The saved spec lock still applies.

On git projects and during the Rosen rollout, the Task List sits in the top-right panel and uses draft (before spec lock), todo, now (an agent is on it), and done (its change landed or PR merged, or someone marked it done).
The Task List counter shows `done/total`.

**Adding tasks (decided):**
- **You:**
  - an always-visible "Add a task" input pinned under the list (Enter adds and keeps focus)
  - `T` from anywhere
  - the + in the panel header
  - `/task <title>` in any composer
  - the palette
- **Agents:** they have full create, read, update and delete over the Task List through `task_list`, `task_add`, `task_update` and `task_delete` (#309).
  They read it at any time.
  Continuous dispatch threads add follow-up work, split work and update or delete tasks without a user request (#439).
  In other threads, agents change tasks only when your message asks them to (e.g. "add these to the task list: X, Y", "mark 3 done", "drop task 5"); otherwise they file issues.
- **Agent tool runtime:** the Task List tools are served by the AIRC MCP server injected into every agent session.
  Its instructions use the thread's saved launch origin on every session start and resume.
  Only the native store owner writes `state.json`:
  - `task_list { query?, offset?, task? }` returns up to 50 tasks per page with their number, title, state, owning thread with its live state, and who added them.
    Filter by title or continue with `next_offset`; specify a task number to continue reading a long title.
    It reads a snapshot the store owner publishes in the project's channel on every pass (about once a second), and before it answers any change, so a change the agent just made is always in it.
  - `task_add { title }`, `task_update { task, title?, state? }` (state `todo` or `done`) and `task_delete { task }` queue a durable project-scoped request under the launcher's identity.
    The store owner applies it, saves the result with thread attribution (duty agents link to their role) and posts a `task` event.
    Dispatch-agent edits and deletes can be undone from their AIRC Events row while the saved result still applies.
    The tool waits up to 5 s for the answer and returns it: the saved task, or why it was refused.
    Past that it returns the queued request sequence to use as a `read_feed` query for the answer.
    Request identities prevent duplicate adds after a crash/restart.
  - An added task is a draft before lock and todo afterward.
    Every task stays a draft until the spec is locked, so a state change needs a locked spec.
    On git projects, a task an agent is on (now) can be marked done and must finish before it can be reopened.
    For Rosen work linked to an active change, the change advances the work state.
    The manual done action remains available for Rosen tasks without an active change.
    Reopening a done task releases its previous ownership and completion, so later events from that attempt leave the reopened task intact.
    Deleting a task drops it from any thread that lists it; the thread keeps running.
  - None of them edits SPEC.md or starts an interview.
- Tasks not created by the spec show who added them: "added by you", or "added by <thread>", which links to that thread.
- Tasks added before the spec is locked are drafts.
- **After the spec is locked** (decided, Q6): a task you or an agent adds goes straight to todo and stays outside SPEC.md.
  Adding a task never starts an interview and never edits SPEC.md.
  When the work changes behaviour SPEC.md describes, the building agent updates the relevant spec in the same change (AGENTS.md).

**Git live state:** a task with an agent shows that agent's state (agent working / agent idle / #n · in review / merged · #n), linked to its thread.
Agent idle means its turn ended before its PR is followed, such as a draft claim not yet marked ready.

**Accept when:**
- [ ] Re-rendering the list never takes focus from the add-task input.
- [ ] New tasks scroll into view.
- [ ] The same Rosen item appears once through claim, name, land, build success, revert and reopen, including after a restart.
- [ ] Abandoning a change frees eligible work and a stack handoff keeps the item with its replacement.
- [ ] Done groups start collapsed, retain older completions and return reopened work to the active list.
