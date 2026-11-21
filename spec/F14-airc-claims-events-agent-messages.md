<!-- split-spec: {"order":1288,"kind":"markdown","indentation":0} -->
### F14 · AIRC (claims, events, agent messages)

One IRC-style channel per project, `#<project>`, shared by every agent in it: thread agents, swarm members, dispatched agents and duty agents.
It exists so agents coordinate instead of working in an echo chamber: they see each other's work, catch feature overlap early, and sequence work that depends on another agent's PR.
It is not meant to be a chat room.
What fills it is **events**, which Velocity posts from what it observes.
**Messages** between agents are allowed but are for the cases events can't express.
Modelled on xros_chat (daily logs, nicks, presence, mentions, ledgers, MCP tools).

**Nicks:** one unique agent name per build thread (`<short-slug>-<thread-id>`, such as `honest-feedback-50` or `issue-532-267`), serving as its AIRC nick, AIRC member identity and `ROSEN_AGENT` (#532).
Duty agents use their role nick (`pr-1`, `scout-1`).
Goal swarm agents use their model's short name and thread number (`sol-1-50`, `argon-2-51`).
Velocity posts as `velocity`.
You post as `you`.

**Issue tools (F9, #535).** `issue_read` and `issue_create` read and file issues on the project's mirror repository using the caller's AIRC identity.
`issue_comment`, `issue_ready_to_close`, `issue_blocked_upstream`, `issue_hold` and `issue_close_not_planned` also require the caller's current claim on `issue:<number>` (#526).
`next_work { sources?, branch?, files? }` atomically claims the first unclaimed, unblocked work item for a self-selecting thread on a spec-locked project and returns its task brief (F9, #539).

**Events.**
Each event is one line: time, a type tag, the nick, and what happened.
Velocity posts them; agents never type them.

| Tag | Posted when | Comes from |
|---|---|---|
| `claim` | an agent's claim is granted, or it extends a claim (e.g. adds an issue its PR also fixes) | `airc_claim` |
| `refused` | a claim loses to an earlier one; names the holder | `airc_claim` |
| `overlap` (amber row) | two working or PR-open agents claim the same files | Velocity's claim comparison |
| `shares` | an agent will make a small change in another agent's file and rebase after it merges | `airc_share` |
| `waits` / `unblocked` | an agent waits for another agent's change, then rebases after it lands | `airc_wait`, Rosen land |
| `PR` · `changes` · `pushed` · `approved` · `merged` | the PR lifecycle (F6, F7) | GitHub PR events saved before #426; Velocity no longer posts them |
| `issue` | a scout or agent files an issue | GitHub |
| `task` | a Task List item is added, changed or deleted (F8) | Task List |
| `failed` | an agent stops on an error; its claim is held | agent runner |
| `discovery` | a Discoveries ledger entry is added | `append_ledger` |
| `swarm` | a swarm or goal swarm starts | you |
| `left` | an agent leaves without a PR (e.g. nothing left to claim) | agent runner |
| `rebased` | a merge touched an agent's files and it rebased (F6) | Velocity |
| `stalled` · `at limit` · `released` | activity changes and claim release (§2.2) | Velocity |
| `proved` | a design or confidence proof checks, with its level per language (F17) | Velocity |
| `statement` | a theorem statement changed after review; the review resets (F17) | Velocity |
| `stress` | a scout's or reviewer's stress script fails; the evidence for an issue or comment (F17) | Scout, review swarm |

**What agents do** (enforced by the AGENTS.md "Coordination (AIRC)" section, F11):
- **Claim before editing.**
  Claim the task, branch and every file they expect to touch.
  Claims on a piece of work go through Velocity's lock (`airc_claim`): the first claim wins, and a later claim is refused with the holder's nick, so the refused agent picks other work.
  The claim stays until the PR merges or the thread is deleted.
  A failed or closed thread keeps holding its claim.
- **Shared files are not contested.**
  AGENTS.md lists them under "Shared files":
  - **Generated files** (lockfiles, generated bindings) are never claimed.
    They are regenerated after every land with `master`, never merged by hand.
  - **Registration lists** (the command, event and module lists every feature adds a line to) take shared claims, which never raise an overlap.

  Without this, every agent overlaps every other on the same few files, and targeted rebases (F6) turn into rebasing everyone.
- **Record overlaps instead of discussing them.**
  When Velocity posts an overlap, the later agent records either a share (`airc_share`) or a wait (`airc_wait`) before it edits the shared file.
  In a wait, its thread shows "waiting for <nick> · #n · shares <file>" and makes no edits.
  When the PR merges, it rebases and an `unblocked` event is posted.
  If the PR is closed, fails or is deleted, the wait ends.
- **Message only when needed.**
  A message is for something events can't say: asking the file's owner for a change, a handoff note after a merge, or answering a mention.
  Agents never restate an event in a message.
- **Address every message.**
  A message names its recipients (`to`) and its `intent`: `inform`, `request` or `question`.
  An agent never replies to an `inform`.
  It answers a `question`, and replies "done" or "declined, because …" to a `request`.
  Agents can't broadcast; only your comments go to every agent.
- **Log discoveries** that other agents need (environment quirks, platform bugs) to the Discoveries ledger.
  Velocity also fills the Discoveries ledger automatically with confirmed post-land failures and their causes (#540).
  Every merge is appended to Past work.
- **Report Velocity's and Rosen's own problems.**
  To file an issue on the project's own repository (including spec questions, contradictions or code findings, even when that project is `mESmaC/velocity`), agents use `issue_create` (#535).
  When a discovery reports a problem in the Velocity IDE/tooling running the session (its worktrees and paths, AIRC, the Scout or review swarm tools, agent shells, the build queue, the code graph or the desktop app), the agent also passes `velocity_issue`, a one-line title of at most 200 characters, to `append_ledger`.
  When the problem is in Rosen itself (its server, workspaces, durability, claims, rebases or landing), it passes `rosen_issue` instead, with the same one-line, 200-character title limit (#303).
  Each discovery targets at most one repository; passing both issue fields is rejected.
  It leaves the issue field out when another agent already reported the same problem.
  Only discoveries carry these fields.
  Every 30 seconds Velocity reads each project's channel and files each marked discovery as an issue on `mESmaC/velocity` or `mESmaC/rosen`, with the agent, project, time and text.
  Before filing, Velocity compares open issues and discoveries recorded within one hour of the report, within the same destination repository (#471).
  It compares titles and discovery text, using file and symbol references resolved through the destination's code graph when available.
  Matching reports add an **Also seen by** comment with the agent, project, recorded time and evidence on the existing open issue.
  A report quotes at most 20,000 characters of the discovery.
  A reported discovery that waits on other issues passes them to `append_ledger` as `blocked_by`, each as `owner/name#n` (#511).
  The entry carries them on a closing `Blocked by:` line, and Velocity links each one to the filed or matching issue as a GitHub issue dependency before it records the report.
  A blocker GitHub refuses, such as an issue that does not exist, is left out and logged, and any other failure is retried on the next pass.
  It uses GitHub's REST API, which keeps working when a swarm has spent the GraphQL budget.
  A per-project cursor and each report's issue number, separated by destination repository, are saved in `velocity-reports.json` in the app data folder; existing Velocity records still load, and a failed report leaves the cursor in place and is retried on the next pass.
  Each report is marked pending before GitHub is asked and recorded as soon as it answers.
  A retried pending report checks for its original issue body or comment before posting again.

**Targeted delivery** (from Google Scion's "Not in `to` → not for you").
At each step boundary, an agent receives the coordination lines addressed to it and relevant project events.
- overlaps that name it, and shares another agent records on its files;
- `conflict` and `updated` on its PR, posted by someone else;
- merges by other agents;
- `broken` events for merges its branch contains (F7);
- events and messages with it in `to`;
- your comments.

Overlap and share notices, unrelated merges and agent messages with intent `inform` wait for the next prompt without starting a turn.
Each prompt groups overlap notices for the same pair of agents into one line listing the files.
Questions, requests, user comments and actionable events can start a turn after a two-second collection window.
Available notices are batched into that prompt, with actionable notices first and the `read_feed` cursor range provided when they exceed the prompt budget.
A legacy PR merge that touches the recipient's claimed or edited files requests a rebase, while Rosen handles native lands through its own follow-through.
GitHub review requests reach the PR author's agent through F6 follow-through, and their AIRC records remain in the log.
An agent is never sent its own events (its claims and claim updates, its PR, pushes, waits, shares and rebases): it caused them, and the tool call that did returned the result.
Routine lines about its work posted by others, such as review coverage, stay in the log.
The event log stays readable on demand with `read_feed` (using `kind: "events"` for event-log entries only, or a specific kind such as `"merged"` or `"rebased"`, #540).
`airc_claims { path?, task?, offset? }` filters held claims by repository-relative path, exact task text or piece key.
With no filter it shows counts by agent, the caller's own claims and claims touching the caller's claimed files.
Each page stays below 2 kB, summarizes oversized rows and returns `next_offset` when more rows are available.
`read_messages` shows up to ten addressed messages or user comments that the session has neither read nor received through step-boundary delivery.
Each preview shows up to 512 UTF-8 bytes.
Call again for more unread messages, or pass a message's `sequence` and `next_offset` as `offset` to read its continuation.
`read_feed` excludes internal delivery acknowledgements, message-mode records, dependency-end plumbing and task-list snapshots, retaining the original sequence numbers for cursor reads.
Entries omit null and empty optional fields, carry uniform monotonic UTC millisecond timestamps, include the Rosen `change` ID on every entry about a change, store structured Rosen event details in typed `payload` fields instead of JSON strings, preserve full claim task text without mid-phrase truncation, and use Rosen terms (`workspace`, `change`) in checkout entries (#532, #540).
`read_feed` and `read_ledger` return newest-first pages with up to 50 entries by default and report how many matching entries remain.
Use `query` to match text, files or symbols, `after` to read newer entries, and the returned `next_before` as `before` for older matches.
Read an oversized entry by sequence and use `next_offset` to continue its JSON.
Before reporting a discovery, search the Discoveries ledger for terms that identify the problem.
This keeps agents heads-down and stops reading costs growing with the square of the agent count.
The modal always shows you everything.

**Message modes** (per agent, set from the nick list or the thread's context menu):
- **Open** (default): receives what targeted delivery sends it.
- **Heads-down:** messages from other agents are held until it opens or updates its PR, or goes idle.
  Events and your comments always get through.
- **Quarantined:** receives nothing from other agents, its messages are delivered to nobody, and its claims are frozen (not released, not extended).
  You can still message it.
  Use it for an agent that is misbehaving.

**Your lines are comments, not prompts.**
They are messages.
A line starting `nick:` is delivered to that agent's thread (shown as "via AIRC") and read at its next step, never mid-step (Rule 1).
Other lines are read by every agent at its next step, including a line that starts with an ordinary word and a colon (`FYI: master is broken`, or a URL).

**A mistyped nick is refused, not broadcast** (decided, #93).
When the single word before the colon is a nick that has left the channel, or is within two character edits of a joined nick (ignoring case), the line is rejected with "Unknown nick `<word>`.
Did you mean `<nick>`?"
and nothing is recorded.
A line meant for one agent never reaches all of them.

**AIRC modal** (footer link or `A`; `Esc` closes):
- **Header:** `AIRC #<project>` and the channel topic.
- **Tabs:**
  - **Events** is the default tab.
    It shows the event log: `hh:mm`, a coloured type tag (blue for PR / issue / task, green for approved / merged / unblocked, amber for overlap / waits / shares / changes / order, red for refused / failed), then the nick and the text.
    Nicks have stable colours, and `#n` references are links that open the thread working on that issue, or the issue on GitHub (F9).
  - **Claims** lists one row per claiming agent: nick, task, state, and its files.
    Overlapping files are outlined amber.
    A dependency shows "waits for <nick> (#n) to merge".
    The tab label shows the overlap count.
  - **Messages** shows agent-to-agent messages and your comments, as `<nick> text`.
    Mentions of you are highlighted.
    Sending a comment switches to this tab.
  - **Discoveries** and **Past work** are the two ledgers.
    A discovery reported to Velocity ends with "Velocity issue: <title> · mESmaC/velocity#n", or "filing…" until it is filed.
    A discovery reported to Rosen uses "Rosen issue: <title> · mESmaC/rosen#n" with the same pending state.
- **Nick list:** grouped On duty / Choosing work / Working / named / Holding a claim / Left (last 4).
  Each nick shows what it is doing.
  Clicking a nick starts a `nick: ` line.
- **Input:** `<you>` and a one-line input.
  Enter sends.

**Storage:** Rosen projects use their Rosen server's channel (R13, D10); git projects keep daily log and ledger files per project under the app data folder.
Agents reach AIRC through MCP tools (`claim`, `release`, `share`, `wait`, `read_feed`, `send_message { to, intent, text }`, `read_messages`, `append_ledger`, `read_ledger`), which the agent runner gives every agent on every provider; the agent runner injects unread mentions at each step boundary.

**Git projects.**
The M8 channel backend is `velocity-airc`: launch `velocity-airc <app-data> <project-id> <member-id> <worktree-leaf-or-duty-nick>` as an MCP stdio server.
Initialization joins the channel; EOF leaves it.
Project identities partition storage independently of display names.
The project-id is the project's store identifier.
Velocity posts events (the F14 tags) as `velocity` on behalf of the nick they name, without joining; an event about a PR also carries its number and head SHA, which the approval record (F7) reads.
UTC daily JSONL logs and the Discoveries/Past work files share monotonically increasing sequence cursors; writes are serialized across processes and flushed before acknowledgement.
Nicks allow up to 48 characters on word boundaries while preserving trailing numeric suffixes, with collision suffixes; `you` and `velocity` are reserved (#532).
Tools available at this stage are `join`, `leave`, `list_nicks`, `read_feed`, `read_messages`, `send_message`, `append_ledger { ledger, text, velocity_issue?, rosen_issue? }`, and `read_ledger`.
The channel also exposes `airc_claims`, `airc_claim { piece?, task, branch, files }`, `airc_release { task }` and `airc_share { holder, file }`.
Claims are durable first-wins records under the channel's cross-process lock, keyed on the piece: a work-pool identity (`task:<id>`, `issue:<n>`, `spec:<section>`, spelled one way whatever the prefix's casing, the spacing or an issue's `#`), or the task text when no piece is given.
The task is display text, so two pieces with the same title never contend.
`airc_release` names the piece or the holder's task.
Refusals name the holder and original time.
Leaving, failure and closed changes retain claims.
Files use repository-relative paths with slash and dot-segment normalization; generated files are omitted.
A file outside the thread's workspace (an absolute path, or one that climbs out with `..`) is never claimed and never overlaps; absolute paths in older logs are ignored.
Registration-list claims use a `::registration` suffix so the rest of the source file remains contested.
New overlaps post `velocity` lines naming both agents; `airc_share` records a share.
When edits are available, notices include symbol details from AIRC worktrees or Rosen snapshots, with the symbols both agents edited called out (F19).
In a Rosen workspace, `graph_overlaps` gives the same details for live path overlaps.
Build threads claim their piece before ACP starts (a dispatched issue, SPEC.md section or Task List item by its work-pool identity, a free-form composer thread by `branch:<its branch>`), so a goal swarm agent and a swarm or dispatched thread on the same item collide in one lock, and a refused build thread fails without starting its agent.
They receive the embedded AIRC MCP server, and extend claims from explicit file writes and Edit/Delete/Move tool locations.
Partial updates inherit the last known kind and locations; Read/Search/Execute/Other and unclassified tool locations do not extend claims.
Successful merge cleanup, thread end or archive, duty scout end and final deletion release the member's pieces and leave the channel (#566).
On startup and on a periodic timer, Velocity reaps pieces and presence whose local member is no longer a live thread or running scout on this machine, while members on other machines keep remote-work rules (#566).
Continuous dispatch ignores `issue:<n>` claims held by known-dead local members, releases them immediately, and reports them in project health as "stale AIRC claim by <nick>, released" rather than "claimed in AIRC" (#566).
Build-thread dependency waits use `airc_wait { holder, file }`: the call records the overlap dependency and remains pending until the runner observes that holder merged, closed, failed or was deleted.
Native filesystem notifications wake the call; it does not poll GitHub.
On merge, the server verifies the waiting thread's registered checkout, fetches the configured base and merges it without rewriting published history, then posts `unblocked` and returns control.
Failed updates retain local work, post `conflict`, and return the failure to the owning agent for resolution.
Closed, failed and deleted dependencies end the wait without updating the branch.
The sidebar shows the holder, PR number when known and shared file.
Claims survive waiting and transport disconnects; reissuing the tool resumes the recorded dependency.
**Parking (M10, §2.2):** once a working or PR-open build thread's wait is recorded and its holder has not ended, Velocity ends the agent's ACP session (its pending `airc_wait` call and AIRC server stop with it), keeps the workspace, claims, transcript and the agent's ACP session id, and sets activity `parked`.
Messages you send a parked thread are held and sent right after its resume prompt.
When the holder merges, closes, fails or is deleted, Velocity itself continues the wait as the call would have (on a merge it updates the branch; a failed update posts `conflict`), posts `unblocked`, and resumes the thread in the same workspace.
Agents that advertise `loadSession` resume through `session/load` without showing the replayed history twice.
Other agents start a new session with the newest excerpt of the earlier transcript within the prompt budget.
The resume prompt says the wait ended and how.
Parked threads resume after an app restart too.
A thread in its delete undo window is neither parked nor resumed.
The optional per-provider cap (F15 "Max agents at once") lives on the Settings → Providers card and ships with it.

**Rosen channel (#269).**
The AIRC tools and modal call the project's Rosen server through `rosen-client`, including through a machine's forwarding daemon.
The MCP connection uses the launcher's `ROSEN_ENDPOINT` when supplied, otherwise the project's configured server endpoint; a disconnection returns the server error and never falls back to local AIRC files.
Member IDs are fixed at launch and scoped on the wire by machine and installation, so two machines' `thread-1` members cannot extend each other's claims; local views retain their original IDs.
The launcher passes its member scope to the MCP process; the tool runtime uses that scope for joins, claims, messages and local projections instead of recomputing it from its provider's environment or path resolution (#411).
Rosen-backed MCP startup rejects an explicit member scope unless it consists of 16 ASCII hex digits followed by a slash (#410, #470).
The native prelaunch claim and the tool connection therefore keep one member and nick, including across reconnects, and the tool can extend its assigned issue claim.
Presence, piece locks, agent messages, message modes and ledgers use Rosen's channel operations.
The pinned protocol carries Velocity issue titles natively; Rosen report text and titles use a versioned discovery envelope in the same server ledger, retaining the server's agent authorship, time and event sequence.
The adapter projects that envelope back to ordinary discovery text and `rosen_issue` for tools, reporting and the modal.
File claims are Rosen path claims (`rosen claim --exclusive <paths>` or the `rosen` MCP `claim` tool with `exclusive: true` for code paths; plain `rosen claim <paths>` notices for registration lists and generated files), shares are `rosen share`, releases are `rosen release`, and parking waits use the `wait` tool exposed on the `rosen` MCP server (#531).
On Rosen projects AIRC keeps work-item identity only: `airc_claim`, `airc_share`, `airc_wait` and `airc_release` are hidden and rejected (as `airc_rebase` is), advance claims omit `branch` and `files` (using `change:<id>` instead of `branch:<slug>` for free-form threads), and observed tool edits do not write AIRC file claims.
When an agent calls `wait` on the `rosen` MCP server and another change holds a path, Rosen records the path wait and returns immediately; once the agent ends its turn, Velocity parks the session and resumes it (applying any pending rebase) when all waited paths are free.
A refused file claim releases a piece newly taken by that call, while an existing piece stays held.
The Claims view reads current server pieces and paths.
Velocity's observed events, registered checkouts, task requests and results, and per-line delivery acknowledgements are versioned records in the server's Past work ledger; the adapter projects them into their original feed kinds instead of showing their envelopes as Past work.
Velocity keeps no local Rosen channel log, claims, modes, presence or ledger copy.
Record appends capture the server event cursor before writing and replay from that explicit cursor after the append response, matching the returned ledger entry to its event sequence (#409).
A ping is not a subscription barrier; simultaneous Goal Swarm setup must not miss its own checkout records.
If the stored record's event does not arrive within 30 seconds, the call reports the stored ledger identifier and asks the caller to read the channel before retrying, without automatically repeating the write.

Rosen's project feed and targeted subscriptions use the server's single R9 event log.
Their delivery cursors advance independently and only after handling; per-line acknowledgements let status notices pass pending input without losing or replaying it.
Message audiences and heads-down/quarantine decisions come from the server; idle and PR updates release held messages.
Dependency calls subscribe from an explicit server cursor before recording or inspecting path waits and retain the original 20 minute deadline across reconnects and parking.
Path releases wake them even when the holder runs on another machine.
A holder's machine publishes its closed, failed or deleted lifecycle to the shared channel; a member absent from another machine's local thread list is never treated as deleted.
Rosen applies workspace updates at step boundaries, so waking a Rosen wait never runs git.

Claim grants and extensions use the atomic claim record itself as their event, formatted with the task and normalized files.
User and agent Task List additions use the same event wording and include the saved task identifier, so replay is deduplicated without hiding separate tasks with identical titles.
Dispatch agents' Task List edits and deletions show Undo on the Events row while the saved result still applies (F8).

AIRC delivery retains each live session’s member nick and cursor.
Review membership is discovered from new joins after its claim; steady-state ticks read only `read(after)`, without copying the whole log or rebuilding presence.
Routing borrows immutable cached history for modes, claims and checkout ownership predating the cursor; durable acknowledgements still let events pass held messages.

M8 message delivery registers live build-thread members using their unique thread agent name (`<short-slug>-<thread-id>`, with legacy `thread-<thread-id>` still recognized for saved threads, #532).
Native delivery adds named messages and your broadcast comments to the next ACP prompt as `via AIRC` notices.
A per-thread cursor in `state.json` advances only after the receiving turn succeeds.
On Rosen projects the saved thread cursor belongs to the server database, and Velocity logs and begins at zero when that database changes or the cursor is past its latest event (#482).
Pending input replays after restart; acknowledged input does not.
Build threads acknowledge each underlying notice after the prompt that carried it completes successfully.
A delivered line is not a message you sent: it never appears in the composer queue, and once the agent reads it the transcript shows it as a "from Velocity" notice.
Heads-down holds agent messages until idle or a PR update, while events and user comments pass through.
Per-line durable acknowledgements prevent replay when held messages keep the scan cursor behind.
Quarantined senders’ messages stay in the modal log but are never pushed; quarantined recipients receive events and user comments.
The modal uses `airc_send` for comments.
Once the review prompt is sent, actionable notices can extend the review, and informational notices ride along with a turn the reviewer is already taking.

**Runtime injection.**
Every session Velocity starts, on every provider, gets an MCP server named `airc` in `session/new`: build threads (composer, dispatch and follow-through resume), Scout sweeps, review swarm sessions and the spec interview.
It runs Velocity's own executable as `velocity --airc-mcp <app-data> <project-id> <member-id> <nick> <member-scope>`, so the server ships inside the app; the standalone `velocity-airc` binary takes the same arguments, with or without the switch.
Four-argument standalone launches remain supported and derive their scope locally.
Member ids are the thread's unique agent name (`<short-slug>-<thread-id>`), `duty-<nick>` (Scouts and review workers) and `spec-<thread id>` (#532).
A build thread uses that same agent name as its AIRC nick and `ROSEN_AGENT`, a duty agent uses its role nick, and the spec interview uses `spec`.

**Acceptance:**
- Two agents that claim the same file get an overlap event within one step, and the later one records a share or a wait before it edits the file.
- PR, review, merge, issue and Task List events appear without any agent posting them.
- A normal run (claim → PR → merge with no conflicts) produces no messages.
- An agent is never sent an event or message unrelated to its claims, PR or waits unless it is in `to` or the line is your comment.
- A heads-down agent receives no agent message until its next PR update or idle, and a quarantined agent's messages reach nobody.
- A dependent agent makes no edits until its dependency merges, then rebases.
- Every merge produces a `merged` event and a Past work entry.
- Your `nick:` line reaches that agent's thread, and the agent replies in the channel.
