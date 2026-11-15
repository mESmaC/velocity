#### PR duty · How a review runs

<!-- split-spec: {"order":996,"kind":"markdown","indentation":0} -->
- **How a review runs.**
  Every 30 seconds, and at once when PR duty starts, resumes or gets agents or a Rosen change is named, lands or is reverted, each idle PR duty agent claims the oldest open, ready PR that needs work.
  A head with no decision gets a review; a head with a standing approval is landed by Velocity (Checks before merge), or by an agent that runs the local checks when no GitHub check arrives; a head with changes requested, or whose approved merge was refused, waits for its author to push.
  A head an agent already finished a session on rests: a decided head until it moves, an undecided one for 5 minutes, after which it is claimed again on its own (Rule 1: never a "needs you" state).
  Instructing PR duty ends every rest at once.

<!-- split-spec links -->

Rule files:

- [Projects never wait for each other, and Rosen work never waits for GitHub](pr-duty-how-a-review-runs-projects-never-wait-for-each-other.md)
- [The claim posts a claim event and a Reviewing · <nick> comment on the PR, which the rail reads](pr-duty-how-a-review-runs-the-claim-posts-a-claim-event-nick-comment-on-the-pr-which.md)
- [A review lasts until its agent records a decision, not one turn](pr-duty-how-a-review-runs-a-review-lasts-until-its-agent.md)
- [A review whose PR is closed or merged by someone else stops on the next pass](pr-duty-how-a-review-runs-a-review-whose-pr-is-closed.md)
- [A review runs in the PR's review-pr-<n> worktree](pr-duty-how-a-review-runs-a-review-runs-in-the-pr.md)
- [The agent records decisions only through Velocity's PR duty MCP tools, served by Velocity's own executable](pr-duty-how-a-review-runs-the-agent-records-decisions-only-through.md)
- [Its card shows #n · <current step>; the feed gets the claim, each approval, request for changes, merge or refused merge, and a review that ended without a decision, with when it is retried](pr-duty-how-a-review-runs-its-card-shows-n-current-step-the-feed-gets-the.md)
- [A message from the PR duty composer goes to each running review as its next message, and to each idle agent with its next claim](pr-duty-how-a-review-runs-a-message-from-the-pr-duty.md)
- [A Rosen change](pr-duty-how-a-review-runs-a-rosen-change.md)
