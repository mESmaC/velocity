#### PR duty · Checks before merge · Why

<!-- split-spec: {"order":983,"kind":"markdown","indentation":2} -->
- **Why:** PR duty agents spent 62% of their time rebuilding and retesting PRs that GitHub CI had already built, on the machine every other agent was compiling on, and 8% reading them.
  Each merge moved the base and restarted the other reviewers' checks: one PR was read in a minute and then checked four times over 40 minutes.
