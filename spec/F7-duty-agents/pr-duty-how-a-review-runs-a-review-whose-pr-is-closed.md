#### PR duty · How a review runs · A review whose PR is closed or merged by someone else stops on the next pass

<!-- split-spec: {"order":1000,"kind":"markdown","indentation":2} -->
- A review whose PR is closed or merged by someone else stops on the next pass: a held PR missing from the open PRs as listed means it is no longer open.
  Its session stops, its claim is released (the AIRC `released` event) and the feed gets "Stopped reviewing #n · it is no longer open", so the agent takes other work instead of checking a PR that can no longer land.
  A review by reading that already approved, or a review whose PR it or Velocity merged, finishes normally and gets no stopped-review line.
  After the session stops, Velocity reads the PR's state: a confirmed merge removes its review worktree and its card's saved path (Rule 5); a close without merging or a failed lookup keeps both.
  A listing that failed or is too old to act on stops nothing.
