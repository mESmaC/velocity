#### Warm pool (speed) · Build queue · Agents request GitHub workflow runs the owner allows

<!-- split-spec: {"order":752,"kind":"markdown","indentation":2} -->
- Agents request GitHub workflow runs the owner allows (#508).
  Agents do not run `gh` directly, so `velocity-run` also offers `workflow_run`, which takes a workflow file name, an optional branch or tag and optional `workflow_dispatch` inputs.
  The owner lists the workflow files agents may request in `rosen.toml`:

  ```toml
  [ci]
  dispatchable = ["ci.yml"]
  ```

  Velocity reads the list from the `rosen.toml` Rosen has in force at the base, never from the agent's workspace, so an agent's edit counts only after it lands.
  A request for a file that is not listed is refused at once, and the refusal names the files the list allows.
  A request from a session without a Rosen workspace, or in a project whose `rosen.toml` names no GitHub `[mirror] remote`, is refused with that reason.
  An accepted request starts at once through the owner's `gh` sign-in, at the repository's default branch unless a branch or tag is given, and the agent ends its turn as it does after `queue_run`.
  Velocity waits for the run for up to six hours and delivers the result in the agent's next prompt.
  The result holds the conclusion, the run's link, each job's result and duration, the failed steps with their annotations and the end of their output, and each artifact's link.
  Velocity downloads artifacts of up to 64 MB each, 256 MB a run, into a folder in the system's temporary directory, beside the full report and the failed steps' whole output.
  Text files of up to 16 KB in a downloaded artifact appear in the result in full, within the prompt's size.
  GitHub has no API for a job's step summary, so a workflow uploads a table it wants an agent to read as a text artifact.
  A thread waiting for a workflow run counts as waiting, as it does in the build queue, so it is never stalled.
  Stopping the session ends the wait and leaves the run going on GitHub.
  Until the pinned Rosen accepts the `[ci]` table, a `rosen.toml` cannot hold the list and every request is refused.
