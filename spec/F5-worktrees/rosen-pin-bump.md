#### Rosen · Pin bumps (#513)

Velocity pins Rosen by commit in its workspace `Cargo.toml`, and keeps that pin from falling behind without the owner asking.
This runs only in a release build of Velocity that has a Rosen project whose GitHub repository is Velocity's own (`mESmaC/velocity`), signed in to `gh` as an account that may push to it.
It needs no thread, agent session or window.

**The look.**
Two minutes after start and every ten minutes, Velocity reads the pin from `Cargo.toml` on the default branch of `mESmaC/velocity`: the `rosen-client` dependency's `git` and `rev`, and `release` in `[workspace.metadata.rosen]`.
It reads the newest 100 commits of Rosen's default branch.
The commits above the pin are ahead of it.
When the pin is not among the 100, Velocity asks GitHub whether the branch contains it, and a pin the branch does not contain is never bumped.
The target is the newest of the 8 newest commits ahead that is green.
A commit is green when neither Rosen's post-land verification nor GitHub CI failed for it, and either (#580):
- Rosen's own post-land verification published `green` covering it, read from this machine's local Rosen server when it runs Rosen's project or from Collapse's post-land read for Rosen's repository; or
- GitHub CI is green: it has at least one check run, workflow run or commit status, every check run and workflow run completed with `success`, `neutral` or `skipped`, and no status is `pending`, `failure` or `error` (#569).

A red result from either source blocks the commit even when the other source is green.
A commit that is blocked or not yet green is passed over for the next older one.

**The issue.**
While a target exists, exactly one open issue titled "Bump the Rosen pin to `<7-character commit>`" asks for it.
Velocity finds pin-bump issues by that title prefix.
The issue says:
- the target commit, the verification source ("green by Rosen's post-land check at `<time>`" or "green on GitHub CI"), how many commits it is ahead of the pin, and a compare link
- the change: set `rev` of every Rosen dependency to the target, set `release` to the target's release tag, regenerate `Cargo.lock` and adapt to Rosen's API changes
- the Rosen issues landed since the pin: every `#N` in the messages of the commits from the pin to the target, with its title
- the open Velocity and Collapse (`mESmaC/collapse`) issues that name one of them as `<owner>/rosen#N`, `rosen#N`, "Rosen #N", "Rosen issue #N" or a link to it
- when the bump is done

A bare `#N` in an issue belongs to that issue's own repository and names no Rosen issue.
When Collapse's repository cannot be read, the issue says so and lists Velocity's issues only.
The issue's last line records the target as `<!-- velocity-rosen-pin-bump:<commit> -->`.

| Open pin-bump issues | What Velocity does |
|---|---|
| none | opens the issue |
| one that says the same | nothing |
| one that says something else | rewrites its title and body, so a newer green commit updates it |
| one whose recorded target the default branch already pins | closes it as completed with a comment, because its bump landed, and opens the issue for the newer target |
| more than one unfinished | keeps the oldest and closes the others as not planned, with a comment naming the kept issue |

While no target exists, Velocity opens and rewrites nothing, and still closes every open pin-bump issue but the oldest.
The open issue is a work item like any other: continuous dispatch (F9) starts it, and its follow-through closes it after the land.

**The Rosen release.**
The target's release tag is `v<Rosen version>-pin.g<7-character commit>`, with the version from `[workspace.package]` in Rosen's `Cargo.toml` at the target, for example `v0.1.0-pin.g7b54fbe`.
The bump writes that tag as `release`; an agent in a Rosen workspace never publishes anything.
On each look, when the default branch's `release` ends in `-pin.g<the pinned commit>` and Collapse's Rosen repository has no complete release by that tag, Velocity builds `rosen` and `rosen-server` from source in a per-revision build folder (#561) and publishes the release, binaries, `rosen-client-commit.txt` and `SHA256SUMS.txt` directly to Collapse without touching any GitHub workflow (#593).
Until they are attached, a Velocity release builds Rosen from source once, as it does for a pin with no release.
A `release` the owner named by hand, or one whose tag names another commit, is never published from here.

**Failures.**
The issue and the release fail separately.
A failed look changes nothing it did not finish and is retried at the next look.
Each failure, and each issue opened, rewritten or closed and each release published, is written to `velocity.log` under `rosen pin`; a failure that repeats is written once.
