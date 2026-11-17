<!-- split-spec: {"order":477,"kind":"markdown","indentation":0} -->
### F4 · Threads and composer

- **Sending from the home composer** starts a thread.
  It gets a Rosen workspace and change from the project base and starts at once.
  The legacy GitHub backend creates a git worktree and branch instead.
- **Thread titles:** the title model (Settings → Providers → Thread titles, F15) names the thread from your first message, as a short imperative title.
  - Velocity asks for the title while it takes a workspace from the warm pool (F5).
    Both finish in under a second with the default model, so the change and workspace are labelled from the title before the agent starts.
  - Until the title arrives, the sidebar shows the first words of your message, pulsing, and the header shows "titling · <model>".
  - If the title model fails or takes longer than 2 seconds, the slug of your message is kept.
  - Threads are titled once, from the first message.
    Later messages don't retitle them.
  - Swarm, goal swarm and dispatched threads take their item's title and don't call the title model.
- **Thread header:**
  - title
  - change
  - workspace path (click → open in Explorer)
  - +/− line counts and age
  - Diff (`D`), Editor (`E`), and `PR #n ↗` once a PR exists
- **Transcript:** user message, agent messages, tool steps (verb, argument, result, running indicator) and diffs.
  User and agent messages render sanitized GitHub-flavoured markdown (headings, lists, links, tables and fenced code), with emphasis shown upright.
  Threads and spec interviews open at the latest content and follow streamed output and new question cards while at the bottom; scrolling up suspends following until the reader returns to the bottom.
  Scrollbars use the reference's slim glass thumb.
- **Composer:**
  - `Enter` sends, `Shift+Enter` adds a new line, `Esc` leaves the box.
  - Model pill (provider, model, effort, and "· Fast" when Fast is on), attach, mic.
    The pill opens a menu grouped by provider: each signed-in provider with its models and limit used, each signed-out provider as a "Sign in with …" row, and effort and speed at the bottom.
    On a running thread, a new model or speed applies from the next message.
  - A message sent while the agent is busy shows "queued · sends when the current step finishes" and a `remove` link.
    The message stays with Velocity, not the agent, until the turn ends, so `remove` withdraws it (the `RemoveQueuedMessage` intent).
    The messages queued during a turn go to the agent together as its next prompt.
    Lines Velocity delivers to the agent (F14, PR follow-through) never join this queue; the transcript shows them as "from Velocity" notices once read.
  - A message the agent never received (no session to resume, a missing workspace) leaves the transcript and goes back to the composer as you typed it, with the error, but only while its thread is still open and the composer is still empty.
    A message the agent received never comes back to the composer, even if saving or redrawing the thread fails afterwards.
- **Slash menu:** `/task`, `/fanout 3`, `/review`, `/compact`, `/model`, followed by the active session's advertised commands and descriptions (`available_commands_update`, §3.2), filtered by the typed prefix.
  Selecting an agent command inserts `/<name>` for the next prompt.
  Advertisements are scoped to the project and thread and cleared when its session is replaced.
- **Skill menu:** typing `$` lists the project's enabled skills with a one-line description each, filtered as you type.
  Picking one inserts `$<skill>`, and the agent is told to load that skill for this message.
  The transcript shows each skill an agent loads as a tool step ("Skill · ui-ux-pro-max").
- **Fan out** (`F`): the same prompt goes to 3 models in 3 workspaces.

**Accept when:**
- [ ] Messages never interrupt a running agent; they are queued (Rule 1).
- [ ] A finished task opens its own PR.
      There is no "Ship PR" button or `/ship` command.

**Rosen-first reference (#414, #422, #437):**
The reference header shows the change ID, workspace path, line counts and running subagent count, with Name change, Rebase and Abandon actions.
Parallel subagents nest below their spawning step as cards with description, model, status, elapsed time, current step and step count.
Opening a card follows its read-only messages, tool steps and diffs in a side panel; closing returns focus to the spawning card.
The sidebar and swarm cards show running subagent counts.
See §5 for panel navigation keys.
