#### PR duty · Tiered review · Reading the diff

<!-- split-spec: {"order":1059,"kind":"markdown","indentation":2} -->
- **Reading the diff:** a change counts as docs, comments or formatting only when Velocity can show it: docs files, whole-line comments, and whitespace that never joins two tokens or sits inside a string.
  Anything else (an unknown file type, a binary, a rename, a mode change) makes the PR normal.
