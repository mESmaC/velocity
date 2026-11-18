#### Scouts · Sweep · Scouts file only through Velocity's scout MCP tools

<!-- split-spec: {"order":1106,"kind":"markdown","indentation":2} -->
- Scouts file only through Velocity's scout MCP tools.
  Search both open and closed issues with `scout_issues { query, limit?, offset? }`, which returns up to ten ranked titles and one-line summaries by default in under 2 kB.
  Read possible duplicates with `scout_issue { number, offset? }`, continuing with `next_offset` for a longer body.
  Find repository labels with `scout_labels { query? }`.
  `scout_file` runs the reproduction, which must fail, attaches the script and output, refuses duplicates, and signs the issue with the scout's name.
