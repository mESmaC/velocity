#### Rosen · Rebases at the step boundary (#271, #398) · AIRC rebase tool (#396)

<!-- split-spec: {"order":600,"kind":"markdown","indentation":2} -->
- **AIRC rebase tool (#396).**
  AIRC omits `airc_rebase` from the tools offered on a project with `rosen.toml`.
  A direct call explains that Rosen applies rebases at step boundaries, without running git or posting a `conflict` event.
  Git projects keep the tool and its existing behaviour.
