#### PR duty · Checks before merge · Review by reading

<!-- split-spec: {"order":973,"kind":"markdown","indentation":2} -->
- **Review by reading** is how a trivial or normal PR is reviewed when GitHub reports at least one check on its head.
  The agent reads the diff and the PR's conversation and records its decision.
  It runs no build, no tests and none of the project's local checks, even where the project's AGENTS.md tells PR duty to.
  Its session ends at its decision.
