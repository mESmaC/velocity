#### Scouts · Sweep · scout_compare runs implementation and compiled Lean reference driver scripts in the scout checkout, with identical UTF-8 cases on stdin (up to 48000 bytes)

<!-- split-spec: {"order":1108,"kind":"markdown","indentation":2} -->
- `scout_compare` runs implementation and compiled Lean reference driver scripts in the scout checkout, with identical UTF-8 cases on stdin (up to 48000 bytes).
  Both drivers must exit successfully; stdout is compared byte for byte, so adapters normalize output.
  It returns both outputs and diagnostics.
  A missing toolchain or failed driver is an execution error, never a model disagreement.
  Scouts preserve disagreeing inputs in a failing script and use `scout_file` to attach executed evidence and check duplicates.
  This backend-only tool never accepts commands from the frontend.
