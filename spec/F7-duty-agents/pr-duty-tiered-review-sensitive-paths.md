#### PR duty · Tiered review · Sensitive paths

<!-- split-spec: {"order":1060,"kind":"markdown","indentation":2} -->
- **Sensitive paths:** each bullet of the AGENTS.md section lists paths, folders or globs as inline code (`*` within a folder, `**` across folders).
  A review reads the list from the base branch's AGENTS.md, so a PR can't lower its own tier.

The tier shows on the rail PR row.
Every tier still needs an approving review, so "never merge without approval" holds.
On a Rosen project that reviews after the land, no tier is reviewed before the land: a sensitive change lands on a clean merge like every other, and goes first in the review queue after it.
