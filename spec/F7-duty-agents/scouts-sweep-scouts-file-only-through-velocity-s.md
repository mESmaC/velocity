#### Scouts · Sweep · Scouts file only through Velocity's scout MCP tools

<!-- split-spec: {"order":1106,"kind":"markdown","indentation":2} -->
- Scouts file only through Velocity's scout MCP tools.
  Search both open and closed issues with `scout_issues { query, limit?, offset? }`, which returns up to ten ranked titles and one-line summaries by default in under 2 kB.
  Read possible duplicates with `scout_issue { number, offset? }`, continuing with `next_offset` for a longer body.
  Find repository labels with `scout_labels { query? }`.
  `scout_file` runs the reproduction, which must fail without hitting package or environment setup errors such as Go `[setup failed]`, attaches the script and output, refuses duplicates, and signs the issue with the scout's name.
  On Windows, scout reproduction and reference commands run with non-verbatim paths when the checkout fits under the short-path ceiling (and a short junction otherwise) and without Velocity's agent shim environment so tools such as Go `//go:embed` match embedded workspace files.
