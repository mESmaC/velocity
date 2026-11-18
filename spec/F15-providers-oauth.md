<!-- split-spec: {"order":1388,"kind":"markdown","indentation":0} -->
### F15 · Providers (OAuth)

Four providers: **Anthropic** (Claude Code agent), **Codex** (Codex CLI agent), **Antigravity** and **Kimi Code** (Kimi CLI agent).
Every one signs in with OAuth on your own subscription, the way T3 Code does.
Velocity talks to each one's agent over ACP (§3.2) and uses the selected account's own login.
It never asks for, shows or stores an API key, and tokens stay in each provider's own credential store.
Any other ACP agent can be added beside the four (Custom ACP agents, below).

**Settings → Providers** has one card per provider:
- Its mark, its name, "runs <agent> over ACP · OAuth", and the ACP agent command (e.g. `claude-agent-acp · stdio`).
  The command and its arguments can be edited, with Reset to return to the built-in one *(proposed)*: adapters get renamed (#46), and you may run a build of your own.
- **Status:**
  - **Signed in** shows how many agents use the provider.
    Each account row shows its email or plan when the provider reports it and lets you rename the account.
  - **Waiting** shows "Waiting for you to finish signing in at <site>…" with Cancel.
  - **Not signed in** means the provider's models are hidden from every picker.
  - **Not installed** is checked again whenever Settings → Providers opens.
    Installing an agent while Velocity runs needs no restart; its card and models update when the check finishes.
    Velocity finds an agent by your saved `PATH` as well as the one it started with, so a folder an installer just added counts, and agents it starts get that `PATH` too.
  - **Install** (#221) on a not-installed built-in provider installs its agent from the provider's own published package, for your user, with no console window.
    The card shows "Installing <agent>…" with Cancel, which stops the install and everything it started.
    When it finishes, the agent is checked again, and a toast says to sign in, or lists its models when its login already holds.
    A failed install keeps Not installed and shows the last lines of the installer's output, with Install offered again.
    Reopening Providers checks the agent again but keeps those lines; a new install, Cancel, a changed command or the agent being found clears them.
    Install is offered only while the card runs its built-in command; an agent you added, or a command you changed, installs outside Velocity.

    | Provider | Windows | macOS and Linux |
    |---|---|---|
    | Anthropic | `npm install --global @agentclientprotocol/claude-agent-acp` | the same |
    | Codex | `npm install --global @agentclientprotocol/codex-acp` | the same |
    | Kimi Code | winget `MoonshotAI.KimiCodeCLI` | `https://code.kimi.com/kimi-code/install.sh` |
    | Antigravity | winget `Google.AntigravityCLI` | `https://antigravity.google/cli/install.sh` |

    On Windows a machine with no npm gets winget's `OpenJS.NodeJS.LTS` first.
    Elsewhere a missing npm fails with a message to install Node.js 22 or later.
    winget's "already installed" counts as success.
- **Accounts:** the existing CLI login is the first account and keeps its current configuration and cached usage.
  **Add account** opens a new OAuth sign-in using a separate CLI profile for each account.
  Claude uses `CLAUDE_CONFIG_DIR`, Codex uses `CODEX_HOME`, Antigravity uses its own Gemini directory and file-based token store, and Kimi Code uses `KIMI_CODE_HOME`.
  The CLI manages its own credentials in that profile.
  An added custom ACP agent uses the login supplied by its command.
- **Default:** Settings sets the default account per provider.
  The composer model menu groups accounts and models by provider and opens on the selected provider's default account unless you pick another.
  The thread header shows the account currently assigned to its session.
- **Sign out:** Velocity records the account as signed out and keeps its CLI credentials so you can sign back in.
  Existing sessions can finish on that account.
  Once the last account signs out, the provider's models disappear from the pickers.
  A signed-out default provider gives way to the first signed-in provider for new composer work.
  Swarms and goal swarms keep the provider selected for each member.
- **Limit:** each signed-in account has a meter and reset times for the windows its CLI reports.
  The composer shows the current account's limit and the model menu shows the limits for other accounts.
- **At the limit** (#444): new threads, dispatch and swarms use the next available signed-in account of the same provider in card order.
  A running build thread that reaches its subscription limit stops its current CLI and resumes automatically on the next available account.
  It keeps its workspace, transcript, unsent messages and any claims it still holds.
  While all accounts are at their limit, a paused thread waits and resumes automatically when an account has room.
- **Max agents at once** (off by default): a cap on concurrently running agents for this provider.
  - Agents over the cap wait as "queued · <provider>" and start when a slot frees.
  - Parked agents (§2.2) don't hold a slot, so waiting on another PR never blocks new work.
  - The card accepts a positive whole number; empty means off.
    Caps apply across projects and roles, including resumed sessions.
    Background thread titles use a separate serial queue so title generation cannot delay a new thread's agent.
    Queued agents start in provider queue order.
    Raising or clearing a cap wakes the queue immediately; lowering it lets running agents finish before admitting more.
- **Models:** chips that are read from the provider after sign-in.
  Clicking a chip hides that model from the pickers or shows it again.

**Default for new threads:** one model card per available model, plus effort and speed.
  A new thread uses these defaults for any choices left unset in the home composer (F4).
Spec interviews, duty agents and swarms choose their models in their own dialogs.

**Reasoning effort by role** (#468).
Settings → Agents sets effort for Implementation, Review, Duty sweep and Title.
Implementation and Title share their saved effort with Settings → Providers, initially High and Low respectively.
Review defaults to Auto, which uses Medium for trivial reviews by reading and High for other reviews, including review swarm work.
Duty sweep defaults to Medium for scout sweeps.
An explicit duty agent effort overrides the role default for that agent.

**Speed: Regular or Fast.**
- **Where available.**
  Some models offer a fast mode, which answers sooner, as Claude Code's fast mode does for Opus.
  Velocity reads whether a model has one from its agent over ACP (§3.2) and offers Fast only there.
- **Where it's set:** everywhere effort is chosen.
  That covers the composer menu, the defaults above, the thread title model, the new-project interview model, and each goal swarm mix row (F16).
  The duty and swarm dialogs use the default speed.
- **Models without a fast mode.**
  On those models the Speed control is greyed out and says "<model> has no fast mode".
  A thread set to Fast that switches to such a model runs at Regular, and goes back to Fast if it switches back.
- **Shown:** the model pill and agent cards add "· Fast" when it is on.
- **Limits** (decided, Q25): fast mode uses the subscription limit a little faster.
  Providers count it inside their ordinary usage, with no category of its own, so the limit bar shows it with no special handling.

**Limit sources** (decided, Q14).
ACP doesn't carry subscription limits.
Each periodic usage report uses the account's own CLI login.
Velocity never stores a token for this.

| Provider | Where it reports its limits |
|---|---|
| Anthropic | Claude Code's usage report (the 5-hour and weekly windows), and the rate-limit events it sends during a turn |
| Codex | `account/rateLimits/read` on Codex's app server, and its rate-limit notifications |
| Antigravity | Google's `retrieveUserQuotaSummary` report, falling back to `fetchAvailableModels`. Uses the selected profile’s token store or the first account’s existing Windows credential. A prompt-free ACP probe asks the CLI to refresh an expired token. |
| Kimi Code | The managed subscription’s `/usages` report at Kimi’s regional API, using the selected CLI profile’s existing OAuth login. Supports the current `.kimi-code` and legacy `.kimi` credential stores. Named quota ratios and older count-based windows supply the usage and reset times. The monthly code share is part of the monthly total. |
| Custom agents | No known source |

- An account with no reported limits shows "limit not reported".
- Antigravity sends the quota User-Agent used by T3 Code and takes the daily host's value when both Google hosts report the same bucket.
  If every daily window looks untouched, it retries the daily report up to three times because the host can return untouched copies of active windows.
  It shows Google’s reset time for each window and omits rolling reset times for untouched windows.
  Older per-model reports without a duration say "reported quota".
  On failure the Antigravity card shows "limit not reported · <short reason>", and Velocity logs request errors and unrecognized response shapes without recording credentials or response bodies.
- Live Claude rate-limit metadata and Codex app-server notifications update the emitting account’s saved meter immediately.
  Codex's Node ACP adapter is launched with a bundled preloader that observes its app-server output without changing the RPC stream or the installed adapter.
  Five-minute reads remain the fallback; a read started before a live update cannot replace that newer report.
  Reports contain only subscription windows, never context-token usage.
- `at limit` (§2.2) never comes from the bar.
  It is set when an agent's turn fails with its provider's usage-limit error, which carries the reset time when the provider gives one.
- Every window a report carries is saved with the reading (Claude's 5-hour and weekly, and its per-model weekly windows; Codex's primary and secondary).
  The fullest one is the headline that the meter, the cards and the pickers show.
  A live event names only its own window, so the other saved windows keep their last read until the next full read.

**Usage modal.**
The footer usage button and the limit meter open a modal, like AIRC's: a centred window over a dimmed, blurred scrim.
It holds thread token usage, the subscription limits laid out like T3 Code's, and the project's code graph section (F19 Measuring it).
Rust draws its rows; the limits update while it is open whenever a limit changes (`layout://state`).
- **Token usage** (#461) totals input, cached input and output for Today and Last 7 days in local calendar time.
  It ranks threads by input plus output and shows totals by project and by agent role.
  Trivial reading reviews and duty sweeps have their own output totals.
  Older entries keep their recorded review or duty role.
  Each row expands into the estimated input from the base prompt, project instructions, code reads, graph calls, searches, diffs, build output, polls, MCP tools, and conversation, with an unattributed remainder.
  Cached input is included in input.
  The thread header opens the thread's lifetime totals, including its title requests.
  Velocity reads explicit per-call ACP token breakdowns when available, otherwise it reads the matching Claude or Codex JSONL from the selected account's profile.
  When only an ACP end-of-turn breakdown is available, it records the reported turn total.
  Session log and ACP records for the same usage are counted once.
  Attribution estimates use the input content visible to Velocity, and the saved ledger holds counts and identifiers only.
- **Usage limits** shows the subscription windows.
- **A section per signed-in account,** in provider card order, with the provider icon and the account’s name and plan.
- **A row per subscription window,** shortest first: the window (`5-hour`, `Weekly`) and "resets in 1d 10h" on one line, then a bar of the share left in the provider's colour and "61% left".
  The tooltip adds the share used and, when the provider gives the window's length and reset time, the share of the window's time left: where even spending would put the bar.
- **A provider with no window to show** (no source, or a failed read) says "No limits reported."
  With no provider signed in, the panel says "No provider is signed in."
- **Code graph** (F19): the active project's On/Off switch and the comparison of its turns with the graph on and off.
  With no project open, it says so.
- **Settings · Providers** at the bottom opens that page.
- Clicking the scrim or Esc closes it.
  Its body scrolls when it is taller than the window allows.

**Custom ACP agents** (decided, Q19).
The four providers are built first.
After them, **Add an ACP agent** takes a name, a command and its arguments, as Zed does.
- The agent gets its own card and works like a built-in provider everywhere a model is chosen.
  Its models and fast mode are read over ACP, and it runs unrestricted with every tool (Rule 1, §3.2).
- It signs in through `authenticate` when it offers an auth method.
  An agent that offers none counts as signed in once `initialize` succeeds.
- The form has no field for an environment variable or a key.
  The agent inherits your environment, as every agent does.

**Thread titles:**
- **Default:** GPT-6 Luna · Low · Fast.
- **Settings:** model cards (signed-in providers only), effort and speed, and a "Reset to GPT-6 Luna · Low · Fast" button when changed.
- **When its provider is signed out:** the section says so, and titles use the first signed-in model at Low until it signs in again.

**Everywhere a model is chosen** (new project, duty dialog, swarm, goal swarm, composer menu, thread titles), only signed-in providers' models are listed.
Each signed-out provider shows as a dashed "Sign in" card.
Clicking it opens Settings → Providers and starts that provider's sign-in.

**Acceptance:**
- No screen anywhere accepts an API key.
- Signing in through the browser updates Velocity without a restart or a manual refresh.
- A signed-out provider's models never appear as selectable.
- Two Claude accounts show independent usage and a dispatch thread resumes on the second account when the first hits its limit.
- A swarm can mix models from several providers, assigned round-robin.
- A goal swarm runs exactly the count you set on each model.
