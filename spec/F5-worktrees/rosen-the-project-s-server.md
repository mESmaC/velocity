#### Rosen · The project's server

<!-- split-spec: {"order":535,"kind":"markdown","indentation":0} -->
- **The project's server.**
  Velocity starts and owns one Rosen server per project, on this machine, with its data in `%APPDATA%\dev.velocity.ide\rosen\<project>` and its own named pipe.
  A release carries `rosen-server` and `rosen` inside Velocity, built from the Rosen commit Velocity pins, and unpacks them on first use into `%LOCALAPPDATA%\velocity-rosen\<content hash>`, so the portable download has them and an update never overwrites a running server.
  A development build finds them in the folder `VELOCITY_ROSEN_BIN` names, beside Velocity's executable, or on `PATH`.
  The first time, it imports the project's history from the checkout's `origin` (the checkout itself when it has none), and `origin` becomes the mirror every land is pushed to (Rosen R1).
  At startup, legacy Git projects migrate one at a time in the background after their running threads finish (#419).
  Migration imports the local default branch and carries the push URL of `origin` over as its mirror.
  Each unmerged branch is imported as a Rosen change with a snapshot for each original commit, and live threads receive Rosen workspace paths while retaining their history.
  The original Git checkouts are kept, and uncommitted work stops only that project's migration with an actionable sidebar notice.
  The saved mode switches only after the import succeeds and the Git checkouts and saved project still match the version that was checked.
  Velocity stops the servers it started when it exits and before an update installs, so their binaries can be replaced.
  A server outlives a Velocity that ended without stopping it, so the first time a run makes a project's server ready it compares the running server's executable with the `rosen-server` it would start itself, by path and then by content (#515).
  A server running another build is replaced before that run starts a watcher or a land on it: Velocity waits for its own lands on that server, sends Rosen's `shutdown` request, waits for the process to exit and starts its own build on the same data folder and endpoint.
  Velocity never ends a server's process, and watchers in other processes journal while the server is away (Rosen R2).
  Only a Velocity that carries Rosen, or is given it by `VELOCITY_ROSEN_BIN`, replaces a server; one that finds Rosen beside its executable or on `PATH` leaves a running server as it is.
  A release writes the Rosen commit its binaries were built from in `rosen-commit` beside them, which is how a build is named; a build without that file is named by its folder.
  Once a server is replaced, a toast says `<project>: Rosen updated to <commit>`.
  When it cannot be replaced, the project header shows `running Rosen <old>; restart to update` with the reason until Velocity restarts, and when Velocity's own build would not start the previous build is started again, so the project keeps a server.
  Velocity follows every Rosen project's `mirror` events, including projects with no threads, and restores the latest mirror state at startup even when its thread-event cursor has passed the failure (#402).
  The saved mirror position belongs to the server database, and Velocity logs and restores it from zero when the database changes or the position exceeds the latest mirror event (#482).
  The first time a run starts a project's server, Velocity catches it up from its mirror before any land can go out, and the project switcher's Catch up from mirror does the same on demand (#501).
  While Velocity runs, it also checks each Rosen project's mirror branch tip right before every land and on a background timer—every two minutes for GitHub mirrors, and for Collapse mirrors starting at two minutes after a land and backing off to 30 minutes after an hour with no lands—plus immediately when Collapse's dashboard reports a newer land (#520, #572).
  The tip check runs a lightweight `git ls-remote` (and a local ancestry check when the base is ahead) without pausing landing; only when the mirror holds commits Rosen lacks does it count how many commits the base trails and run Rosen's catch-up.
  While a catch-up is pending or running, the project header and the Rosen status pane's Mirror section show `Behind the mirror by N`, naming both tips and offering Catch up now when waiting to retry.
  When a mirror push is refused because the remote is ahead and the server's base is an ancestor of that tip, Velocity runs Rosen's catch-up and wakes the mirror push worker instead of raising a `rejected · needs a person` notice or AIRC entry (#520).
  Rosen's `mirror catch-up` (Rosen #416) fetches the mirror branch and, when the base is an ancestor of its tip, moves the base there and rebases open changes, whose agents are told as for any base move.
  Catching up keeps the server's database, with its events, cursors and channel history; Velocity never re-imports or replaces a database to catch up.
  A base that matches or is ahead of its mirror is left as it is, and a project with no mirror has nothing to catch up.
  When the base moves, a toast names the project, the commits caught up and the open changes rebased; the action also reports when nothing moved.
  A catch-up on start that fails, such as a mirror fetch with the network not up yet, raises one toast naming the project and is tried again after 30 seconds, then at waits that double up to ten minutes, until Rosen answers a catch-up, and a toast then says how it ended.
  The failure and each new reason are written to the diagnostics log, and the project's action ends the retries when it gets an answer first.
  When the histories have diverged nothing moves, and the project header's mirror notice shows both tips and Rosen's list of the commits on each side, until a later catch-up or an adoption succeeds.
  The notice offers Adopt mirror tip, which adopts the mirror tip as Rosen #396 does when the mirror's edits were already ported into Rosen, and shows Rosen's refusal otherwise, and Catch up again.
  While the latest push has failed, the main window's project header and Settings → Workspaces → Version control show `failing · retrying` with the next retry time, or `rejected · needs a person` when Rosen says the push was rejected.
  Both show the error's first three lines and the last commit the mirror accepted, or say that no accepted commit is recorded.
  The first rejection in an incident raises one toast naming the project and writes one `failed` entry in its AIRC channel, addressed to the current duty agents.
  Retrying the same rejection does not repeat the toast or channel entry, including after a Velocity restart; a channel failure leaves a durable notice to retry without hiding the project warning.
  The warning clears on the next successful push, including a recovery after a server restart, and a later rejection starts a new incident.
  When the mirror remote is a Collapse repository, Velocity saves its `owner/repository` on each mirror refresh and offers Open on Collapse (#429).
  The project switcher menu and the palette open the repository page at `https://collapse.sh/<owner>/<repository>`.
  A standing land opens its land page at `…/land/<commit>`, which Collapse caches as immutable, from the Rosen status pane's Changes, the thread's land history, AIRC land events and the palette entry for the selected thread (#573).
  A project without a Collapse mirror shows none of these actions.
  In the reverse direction, Velocity registers the `velocity://` scheme on install and keeps a single running instance so links from Collapse reach the running window (#479).
  Opening `velocity://collapse/<owner>/<name>` focuses the local project whose Collapse mirror matches `<owner>/<name>` and opens an unsent draft thread in it.
  Opening `velocity://collapse/<owner>/<name>/land/<commit>` does the same and attaches `Land <short commit>` as dismissible context on that draft.
  When no local project matches, Velocity checks the connected Collapse account's dashboard (#575).
  An unlinked repository opens through the same Open from Collapse path (F2), cloning with its push token and proactive auth, importing into Rosen, setting `[mirror]` and the `collapse` root, and applying the relay allow-list.
  When the repository already has a linked server (or is unavailable from the connected account), Velocity changes nothing and shows a notice naming the repository that offers machine enrollment with a join key from the project's owner (Settings → Workspaces → Machines).
  Rust parses every link, ignores unknown paths and query parameters, and never sends a message, starts an agent, runs a command or passes link text into a prompt or a shell.
  A Rosen project without a Collapse mirror offers Mirror to Collapse in the project switcher menu (#427).
  It provisions a Collapse repository named after the project on the project's base branch as Create does (F2), then proposes and lands one `rosen.toml` change setting `[mirror] remote` to `https://api.collapse.sh/<owner>/<repository>.git` and the top-level `collapse` root to `https://api.collapse.sh/<owner>/<repository>` (#517).
  A refused land leaves the change open and the toast names it with Rosen's reason.
  `[mirror] remote` sends landed commits only; the `collapse` root is what Rosen's state feed, ledger push and action relay read (Rosen #395, #403, #404, #406).
  With it Rosen sends Collapse its open changes, review, coverage, configuration, post-land events, machines and presence, sends ledger entries, and fetches the actions the owner queues on Collapse.
  Collapse authenticates all three with the repository's push token, and Rosen asks git's credential helper for it under `<collapse>/rosen/state` and `<collapse>/ledgers`.
  Per-path credentials key those apart from the mirror URL, so Velocity stores the same push token under all three URLs, and it stays in the credential helper, never in `rosen.toml` or `state.json`.
  Private Collapse repositories answer unauthenticated Git reads with 404 rather than a login challenge, so Velocity also sets `http.https://api.collapse.sh/.proactiveAuth` to `basic` in the user's global git configuration whenever it provisions Collapse credentials (Create with Collapse, Mirror to Collapse and Collapse sign-in), requiring Git 2.46 or later (#571).
  Rosen's relay runs only the kinds the server's operator allowed.
  Velocity owns the project's server, so linking defaults to every kind Collapse offers (`approve`, `land`, `mark`, `config` and `revoke`), and the toast names them.
  Each project's relay allow-list is saved in Velocity's state and re-applied whenever Velocity starts a project server whose base `rosen.toml` names `collapse`, so a rebuilt or re-imported server still runs relayed actions (#576).
  Each relayed action still passes Rosen's gates and is recorded as `<account>@collapse`.
  Settings → Workspaces includes a Collapse section for each Rosen project showing the `collapse` root, push token status in git's credential helper, state feed and ledger push health (last sent and last error), a channel lines toggle (`collapse_channel`) that lands a `rosen.toml` change, relay action kind checkboxes, and Unlink (#576).
  Unlink removes the `collapse` root (and `collapse_channel`) through a landed `rosen.toml` change without altering `[mirror] remote`, clears the server's relay allow-list, and stops the feed.
  A project that already mirrors to Collapse without the `collapse` root offers Send Collapse its live state in the project switcher menu and in the Rosen status pane's Mirror section.
  It copies the push token the credential helper holds for the mirror to the two feed URLs, or mints one when the helper holds none and Collapse is connected, then lands a `rosen.toml` change setting only `collapse` and allows the relay.
  The pane offers the same action while the state feed is failing, where it stores the token again and leaves a `rosen.toml` that already names the root as it is.
  For a project that has a GitHub repository (`github_remote`), Mirror to Collapse and Move to Collapse call Collapse's downstream API (`POST /api/downstream/{owner}/{name}` with `{"github_repository": "<owner/repo>"}` only and `GET /api/downstream/{owner}/{name}` to read the state back, Collapse C4) so lands reach GitHub through Collapse without a browser step (#579, #611).
  If the downstream cannot be set, Velocity marks Move to Collapse's downstream step Failed, surfaces `GitHub copy is not being updated: <reason>` in the title bar and project health with a link to `https://collapse.sh/<owner>/<name>/settings`, opens the App setup page when Collapse reports a missing GitHub App or permissions, and clears the warning once Collapse's forward state reports in sync (#579, #611).
  When the project has no GitHub repository, forwarding is left alone.
  While any project mirrors to Collapse, Velocity reads Collapse's dashboard with the personal token after each mirror push, on window focus, when the Rosen status pane opens, and once a minute for five minutes after the last land, and saves each repository's last land and forwarder state with the project (#572).
  The Rosen status pane's Mirror section shows them (F6), and the project header warns while the forwarder is stopped or the repository is archived, with Open on Collapse to retry there.
  Beside them the section shows the state feed: `Collapse receives commits only` without the `collapse` root, otherwise when it last sent, and `State feed failing` with Rosen's error while its latest pass failed or stopped.
  Velocity reads the state feed and ledger push send times and errors from Rosen's `ServerStatus.collapse` (`CollapseHealth.*.last_success_at` and `last_error`) so the displayed send time reflects the actual successful delivery rather than an acknowledged event's creation timestamp (#662).
