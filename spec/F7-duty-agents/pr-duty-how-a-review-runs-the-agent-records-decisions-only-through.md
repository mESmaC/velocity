#### PR duty · How a review runs · The agent records decisions only through Velocity's PR duty MCP tools, served by Velocity's own executable

<!-- split-spec: {"order":1002,"kind":"markdown","indentation":2} -->
- The agent records decisions only through Velocity's PR duty MCP tools, served by Velocity's own executable: `pr_duty_pull_request` reads the PR, `pr_duty_review` approves or requests changes at the head it checked (the approval record above), and `pr_duty_merge` merges the approved head with the role's merge method.
  In a review by reading, `pr_duty_review` approves without a check receipt, refuses while a review thread is open or when the head changes proof files (that head needs the local proof checks), and `pr_duty_merge` refuses: Velocity merges.
  The launcher fixes the agent's nick and its one PR, so it can't act on another agent's PR or record a decision as anyone else.
