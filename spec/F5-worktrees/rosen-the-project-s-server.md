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
  Velocity follows every Rosen project's `mirror` events, including projects with no threads, and restores the latest mirror state at startup even when its thread-event cursor has passed the failure (#402).
  The saved mirror position belongs to the server database, and Velocity logs and restores it from zero when the database changes or the position exceeds the latest mirror event (#482).
  The first time a run starts a project's server, Velocity catches it up from its mirror before any land can go out, and the project switcher's Catch up from mirror does the same on demand (#501).
  Rosen's `mirror catch-up` (Rosen #416) fetches the mirror branch and, when the base is an ancestor of its tip, moves the base there and rebases open changes, whose agents are told as for any base move.
  Catching up keeps the server's database, with its events, cursors and channel history; Velocity never re-imports or replaces a database to catch up.
  A base that matches or is ahead of its mirror is left as it is, and a project with no mirror has nothing to catch up.
  When the base moves, a toast names the project, the commits caught up and the open changes rebased; the action also reports when nothing moved.
  When the histories have diverged nothing moves, and the project header's mirror notice shows both tips and Rosen's list of the commits on each side, until a later catch-up or an adoption succeeds.
  The notice offers Adopt mirror tip, which adopts the mirror tip as Rosen #396 does when the mirror's edits were already ported into Rosen, and shows Rosen's refusal otherwise, and Catch up again.
  While the latest push has failed, the main window's project header and Settings → Workspaces → Version control show `failing · retrying` with the next retry time, or `rejected · needs a person` when Rosen says the push was rejected.
  Both show the error's first three lines and the last commit the mirror accepted, or say that no accepted commit is recorded.
  The first rejection in an incident raises one toast naming the project and writes one `failed` entry in its AIRC channel, addressed to the current duty agents.
  Retrying the same rejection does not repeat the toast or channel entry, including after a Velocity restart; a channel failure leaves a durable notice to retry without hiding the project warning.
  The warning clears on the next successful push, including a recovery after a server restart, and a later rejection starts a new incident.
  When the mirror remote is a Collapse repository, Velocity saves its `owner/repository` on each mirror refresh and offers Open on Collapse (#429).
  The project switcher menu and the palette open the repository page at `https://collapse.sh/<owner>/<repository>`.
  A standing land opens its commit page at `…/commit/<commit>`, which Collapse caches as immutable, from the Rosen status pane's Changes, the thread's land history, AIRC `merged` events and the palette entry for the selected thread.
  A project without a Collapse mirror shows none of these actions.
