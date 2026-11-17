#### Main watcher

<!-- split-spec: {"order":1071,"kind":"markdown","indentation":0} -->
**Main watcher** *(role name proposed)*: a duty role that watches `master` after merges, modelled on Anthropic's "superfan" agents.

For a Rosen land with a source issue, Velocity closes on behalf of the main watcher once 30 minutes have passed since verified R12 build success and the mirror carries the landed commit.
It uses the durable build-success prefix with a nonempty build check set covering the landed SHA.
An active review claim or an objection on that exact land prevents fallback.
The review swarm's positive verdict takes precedence and a revert reopens the issue with the culprit output.
The Close issues option controls closure and reopening for issue swarms.
Issue follow-through runs independently of landing (#441).

<!-- split-spec links -->

Rule files:

- [After each merge it runs every scout test and stress script (F17)](main-watcher-after-each-merge-it-runs-every.md)
- [When master breaks, it bisects the recent merges, posts a broken event naming the merge, and opens either a fix PR or a revert PR](main-watcher-when-master-breaks-it-bisects-the.md)
- [Targeted delivery (F14) tells only the agents whose branches contain the breaking merge](main-watcher-targeted-delivery-tells-only-the-agents.md)
- [It tracks flaky tests and files issues for them](main-watcher-it-tracks-flaky-tests-and-files.md)
- [Never push to master; every fix or revert is a PR](main-watcher-never-push-to-master-every-fix.md)
- [How a watch runs](main-watcher-how-a-watch-runs.md)
