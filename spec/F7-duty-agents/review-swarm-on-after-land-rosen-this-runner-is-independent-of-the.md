#### Review swarm on after-land Rosen projects (#382) · This runner is independent of the landing checker

<!-- split-spec: {"order":1091,"kind":"markdown","indentation":0} -->
- This runner is independent of the landing checker.
  Reading coverage, launching workers, filing findings and checking Lean never become land prerequisites.
  The release gate remains responsible for full tests and proofs.
