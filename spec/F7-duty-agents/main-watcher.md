#### Main watcher

<!-- split-spec: {"order":1071,"kind":"markdown","indentation":0} -->
**Main watcher** *(role name proposed)*: a duty role that watches `master` after merges, modelled on Anthropic's "superfan" agents.

<!-- split-spec links -->

Rule files:

- [After each merge it runs every scout test and stress script (F17)](main-watcher-after-each-merge-it-runs-every.md)
- [When master breaks, it bisects the recent merges, posts a broken event naming the merge, and opens either a fix PR or a revert PR](main-watcher-when-master-breaks-it-bisects-the.md)
- [Targeted delivery (F14) tells only the agents whose branches contain the breaking merge](main-watcher-targeted-delivery-tells-only-the-agents.md)
- [It tracks flaky tests and files issues for them](main-watcher-it-tracks-flaky-tests-and-files.md)
- [Never push to master; every fix or revert is a PR](main-watcher-never-push-to-master-every-fix.md)
- [How a watch runs](main-watcher-how-a-watch-runs.md)
