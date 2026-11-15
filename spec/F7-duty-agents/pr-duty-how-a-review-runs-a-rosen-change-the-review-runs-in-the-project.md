#### PR duty · How a review runs · A Rosen change · The review runs in the project's review-change-<thread>-<nickname> folder

<!-- split-spec: {"order":1032,"kind":"markdown","indentation":4} -->
- The review runs in the project's `review-change-<thread>-<nickname>` folder: the change's merge result onto the base, written by Rosen, with the per-agent setup a worktree gets (a review by reading gets no setup).
  It is a plain build tree, not a git checkout, and it is removed when the review ends.
  A review that runs the checks gets the folder from Rosen's warm pool when it can (Rosen R3): a ready workspace brought to the merge result, with the build output its prepare left, so the review's first build compiles what the change touched and not the whole project.
  A review by reading gets the files alone and takes nothing from the pool.
