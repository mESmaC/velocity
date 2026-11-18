#### Review swarm on after-land Rosen projects (#382) · A worker claims the oldest unreviewed landed change through Rosen's channel, with sensitive changes first

<!-- split-spec: {"order":1086,"kind":"markdown","indentation":0} -->
- A worker claims the oldest unreviewed landed change through Rosen's channel, with sensitive changes first.
  It starts with the landed file list, diff stats, and the code graph's changed symbols and impact in the parent and landed trees (F19).
  It reads the author's named snapshots, the referenced issue and the implemented SPEC section, and requests the exact landed patch one file at a time with `review_swarm_change { path, offset? }`.
  It checks every implementation claim and uses `next_offset` to continue a large file's patch as needed.
  A channel review claim names the change and landed commit; it holds no source path or landing lock.
  Workers claim paths and issues before writing proofs or fixes.
