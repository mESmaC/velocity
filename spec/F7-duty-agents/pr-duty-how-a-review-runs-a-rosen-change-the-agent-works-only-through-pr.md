#### PR duty · How a review runs · A Rosen change · The agent works only through PR duty's Rosen MCP tools, served by Velocity's own executable

<!-- split-spec: {"order":1034,"kind":"markdown","indentation":4} -->
- The agent works only through PR duty's Rosen MCP tools, served by Velocity's own executable: `rosen_duty_change` reads the change (diff, named snapshots, review threads), `rosen_duty_prepare_landing` writes its current merge result into the folder, `rosen_duty_checked` submits the receipt for that exact head and base once the proofs check on it (F17; `lake build` and the audits when the change touches the Lake project), `rosen_duty_review` approves (in Rosen, after the receipt), requests changes, comments or holds, and `rosen_duty_land` lands the approved head.
  The agent approves and lands as `pr-duty-<nick>`.
