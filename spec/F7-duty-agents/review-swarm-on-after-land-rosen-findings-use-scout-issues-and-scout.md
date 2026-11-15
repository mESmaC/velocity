#### Review swarm on after-land Rosen projects (#382) · Findings use scout_issues and scout_file

<!-- split-spec: {"order":1087,"kind":"markdown","indentation":0} -->
- Findings use `scout_issues` and `scout_file`: open and closed duplicate checks, an executed failing reproduction and attached output.
  A channel claim serialises publication across workers.
  A worker may instead write a fix-forward change.
  It never reverts by hand; R12 owns recovery, and in after-land mode only a broken build is reverted.
