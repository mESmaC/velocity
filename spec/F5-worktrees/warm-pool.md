#### Warm pool (speed)

<!-- split-spec: {"order":672,"kind":"markdown","indentation":0} -->
**Warm pool (speed):**

**Rosen projects (#272, #418, rosen#31).**
Rosen owns the ready-workspace pool and its preparation worker.
The project's `rosen.toml` `[warm_pool]` size sets its standing target; `[prepare]` commands install pinned toolchains, fetch dependency caches and prebuild the template (block-cloned on ReFS), or each slot on other volumes.
Rosen preserves preparation across its prose-only lands and retries failed preparation after the base moves.
After each take at the final path, Velocity copies the configured untracked files, links enabled skills into the provider folders, then runs the project's after-take setup command with its compiler-cache setting.
These steps run even when Rosen reports an unprepared take; a failed setup keeps the workspace and prevents the agent from starting.
Reopened and stacked workspaces run setup at their new path before resuming, and a successful local setup marker prevents repeated setup on ordinary session resumes.
Rosen reviews that run checks and review-swarm workers get the same setup.
A swarm or goal swarm raises the local machine's Rosen pool target to at least its agent count and the configured standing size before launching members; a later smaller launch never shrinks that target during the run.
Rosen owns refill, preparation, prose-only reuse and pool takes; Velocity does not run git in those workspaces.

<!-- split-spec links -->

Rule files:

- [Skills](warm-pool-skills.md)
- [Lean](warm-pool-lean.md)
- [Swarm launches](warm-pool-swarm-launches.md)
- [Build queue](warm-pool-build-queue.md)
