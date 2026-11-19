#### Post-land verification

Velocity asks Rosen to land a named change on a clean merge with the base.
Rosen projects opened in Velocity use `review = "after-land"`.
Rosen status offers the one-line configuration change if the base still specifies `review = "before-land"`.
The notice leaves the project usable.

Rosen R12 owns the queue of verification jobs, bisection, confirmed failures and automatic reverts under its configured policy.
Velocity runs Rosen's commands on the requested commit in a reusable build folder and reports their results.
The worker runs independently of Scouts and the review swarm.
Velocity's batched integration build runs compilation, lint and full tests, and a confirmed broken build can start a fix-forward thread.
A revert returns the change and failing output to its author.

Velocity can close a source issue 30 minutes after verified R12 build success when the mirror carries the landed commit and the exact land has no review claim or objection.
A positive review swarm verdict takes precedence, and a revert reopens the issue with the culprit output.
The Close issues option governs issue swarms.
