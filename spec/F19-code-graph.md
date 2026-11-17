<!-- split-spec: {"order":1752,"kind":"markdown","indentation":0} -->
### F19 · Code graph

**What it is:** Velocity maps each project's code into a graph of symbols, files and subsystems.
Every agent queries it through tools, so it finds the right code with fewer searches and reads only the lines that matter.
The aim is less digging: fewer tool calls and fewer tokens per task at the same accuracy.
It is built into Velocity, needs no install, and keeps one graph per project for all of the project's agents.
Inspired by [Graphify](https://github.com/Graphify-Labs/graphify) (#277).
Its accuracy rules come from the A.TXY address memory in MCL4-COE and `elem`: route hard, process soft.

**Accuracy rules:**
- **Exact or a classified failure.**
  A lookup returns one exact record, or `ambiguous` (with every candidate), `missing` (with the nearest names) or `stale`.
  It never guesses between candidates, and a name that matches several symbols is never resolved to one of them.
- **The graph points; the agent reads.**
  Every record gives its file, line range and signature, so the agent opens exactly those lines.
  The graph never stands in for the source.
- **Never stale.**
  Every record carries the hash of the source span it came from, and the graph the hash of every file.
  Before an answer leaves, the files it names are hashed again; a file that changed is re-indexed and the answer is made again from the new records.
- **Every edge says how it was found.**
  `extracted` edges are explicit in the source: a definition's container, an import, a call to a name defined in the same file or imported into it, a path-qualified call (`Type::new`, `module::run`).
  `inferred` edges are resolved by name across the packages the caller's package depends on, with one candidate.
  A name that matches several definitions is `ambiguous`, kept with its candidates.
  A method call on a receiver whose type the calling code never names is `unconfirmed`.
  Programs and impact never follow `ambiguous` or `unconfirmed` links unless asked.
- **No guessing by common names.**
  Several rules keep name matches from inventing links (after Graphify's resolver):
  - A standard type name (`String`, `Result`, `Error`) links only to a definition in the same file or an imported one.
  - A standard method name (`get`, `len`, `join`, `push`) on a receiver of unknown type links to nothing.
  - Test, example and stress code is never a candidate for production code.
  - In JavaScript and TypeScript, a name links only through a definition in the file or an import.
  - A call inside a macro's arguments is `inferred` at best.
- **No tool is restricted (Rule 1).**
  Agents keep Read, Grep and the shell.
  The tools' instructions say when the graph is the better path (finding a symbol, its callers, what a change affects) and when it isn't (strings, literals, config values, build output).
- **No model in the index.**
  Parsing and resolution are deterministic (tree-sitter), so building the graph spends no subscription usage.
  Docs are read for structure only.

**What it holds** (planes, after A.TXY):

| Plane | Records | Links |
|---|---|---|
| 1 · Symbols | functions, methods, types (struct, enum, trait, class, interface), constants, macros, Markdown headings | UP to the file; `calls`, `implements`, `references`; `implements` from a trait method's implementation to its declaration; `calls` from frontend code into a Tauri command (`commands.checkForUpdate`, `invoke("check_for_update")`); `mentions` from a doc heading to the code its section names in backticks |
| 2 · Files | each tracked source and Markdown file | UP to the subsystem, DOWN to its symbols; `imports` to the files and the symbols it imports (through `export * from` barrels) |
| 3 · Subsystems | workspace members and packages (Cargo members, `package.json` workspaces and packages, Go modules) and otherwise top-level folders | DOWN to their files; `depends` from the manifests |

- **Languages:** Rust, TypeScript, JavaScript, Svelte (its script blocks), Python, Go, C#, Java, C and C++, plus Markdown headings.
  Other files are listed at plane 2 with no symbols.
- **Left out:** files git ignores, files git doesn't track, and the project's generated files (lockfiles and generated bindings, as AIRC's claims leave them out, F14).
- **Addresses:** each record has a short address such as `1.k3f9q`: its plane, then base-36 characters from a hash of its path, qualified name and kind.
  An address stays the same while the symbol keeps its name and file, so an agent can pass it back in later calls.
  A moved or renamed symbol returns `stale` with the record's new address when one matches.

**Building and keeping it current:**
- **Parse cache.**
  Velocity keeps one parse cache per project in the app data folder: each file's symbols and references, keyed by a hash of the file's content, the parser version and Velocity's version.
  It is an append-only log of checksummed records, shared by every agent of the project, so a file is parsed once whatever workspace or commit it appears in.
  A torn or corrupt record at the end of the log is dropped and that file is parsed again.
  The log is compacted when it grows past twice the size of the records still in use.
  A large parse (a new project, a cold cache) runs under the project's build lock, so a swarm's agents starting together parse once and the rest read the cache.
- **Each agent's graph.**
  An agent's tools hash every file in its worktree that git tracks or doesn't ignore, take what the cache already holds, parse the rest, and resolve names across the whole set.
  In Rosen workspaces, the index walks the workspace files using the working tree's ignore rules.
  A file watcher batches edits and refreshes the workspace graph in the background.
  A tool call refreshes pending changes before answering, and the span check catches edits the watcher missed.
- **Warming.**
  After each merge to the base branch, Velocity parses the files the merge changed, read from git at the new tip, in the background below normal priority on the trigger that catches up the warm pool (F5).
  New agents then start with a full cache.
- **Speed:** a query answers in under 50 ms on a 100,000-line project.
  Building the graph for such a project takes under 10 s with an empty cache and under 1 s with a full one.

**Agent tools.**
Every session Velocity starts, on every provider and in every role, gets an MCP server named `velocity-graph` in `session/new`, beside `airc` (§3.2, F14).
The name carries Velocity's reserved prefix, so it never clashes with a server of your own, such as Graphify's.
It runs Velocity's own executable as `velocity --graph-mcp <app-data> <project-id> <worktree>`.

| Tool | Does |
|---|---|
| `graph_find { query, plane?, kind?, limit? }` | Ranks candidates for a name or a plain-language description. Matching is by words, with camelCase and snake_case split, word stems and rarer words weighted higher. A record's score scales with the square of the share of the query's words it matches. An exact name ranks first, then names that start with the query. Tests, examples and docs rank lower unless the query asks for them. Returns addresses with kind, name, file:line and signature. A result that is the only exact match for the name is marked `exact`. |
| `graph_read { target, source? }` | One record by address or exact name: signature, doc summary, file and lines, its UP chain, its DOWN children by name, and its links grouped by relation with their counts. `source: true` adds the source lines themselves, so the agent doesn't need a separate read. Fails as the accuracy rules say. |
| `graph_impact { targets, depth? }` | What depends on symbols or files, followed transitively through calls, implementations, type uses, imports and doc mentions (2 hops by default, at most 6). A type brings its methods and a file its symbols, and an implementation of a trait method brings the callers of the trait method. A hub (a record with at least the 99th percentile of incoming links, and at least 50) is listed but not followed. `ambiguous` and `unconfirmed` links are listed only as direct leads. The answer is grouped by subsystem with depth, and links found in the source are listed apart from leads to check. It says that no dependents doesn't prove code unused. |
| `graph_run { program }` | A pointer program of up to 16 steps, run in one call over one register: `FIND`, `AT`, `READ`, `UP`, `DOWN`, `FOLLOW <relation> [in\|out] [all]`, `FILTER <kind\|plane\|path glob\|name>`, `PATH <target>`, `COUNT`, `SOURCE`. Returns the result and a one-line audit for each step (what it did and how many records it held). `PATH` lists each hop with its relation and provenance, and the path's confidence: that of its weakest hop. It never passes through a hub. For example, `FIND "approval record"; FOLLOW calls in; FILTER path crates/velocity-github/**; COUNT` replaces several searches and reads with one call. |

- **Budget.**
  Every answer is capped at 8,000 characters.
  A cut answer says so on its first line, with how much was kept and how to narrow it.
- **Instructions.**
  The server's MCP instructions state when the graph is the better path (where something lives, what calls it, what a change affects) and when grep is (exact text), that a record's lines are what to read, and that `inferred`, `ambiguous` and `unconfirmed` links are leads to check.
  They are written as facts, not orders: imperative rules read like prompt injection and get ignored.
- **Session-start map.**
  When the index is ready within 1.5 s of the agent connecting, the instructions end with a map of about 250 tokens: the workspace, its file and symbol counts, its biggest subsystems and its most-used symbols with their addresses.

**Measuring it.**
The graph exists to save time and tokens without costing accuracy, so Velocity measures it per project and shows reported token counts by thread.
- **The switch.**
  Each project has a code graph switch, on by default, in the usage modal.
  Agents started while it is off get no `velocity-graph` server; agents already running keep what they started with.
  It is saved with the project's graph in the app data folder, and warming skips a project whose graph is off.
- **Index status.**
  Opening Usage checks the active project's base commit and starts a background index if the graph is enabled and the indexed base differs.
  The panel shows Indexing with progress, Ready, Stale, or Failed with the indexing error and Retry.
  It retains the most recent successful base coverage, including its commit and update time, files and symbols by parser, edges by source language, and counts of skipped files by reason.
  It also shows active thread workspaces that are indexing or refreshing, the shared parse log's size on disk, and the share of parses reused from cache during the last base update.
- **Tool calls.**
  Each tool shows its calls and the shares that resolved an exact lookup or returned a classified lookup failure.
  Ranked matches and request errors are also counted in the total.
  The modal compares graph calls with raw source reads and searches observed in ACP tool events, and shows input, cached input, output and observed raw reads by thread workspace.
  Availability of raw read counts and token totals depends on the events and turn usage the provider reports.
- **Totals.**
  Every turn of every agent, in every role, adds its tokens and its working time to the project's totals for the side it ran on: the graph on or off.
  Tokens are what the agent reports when the turn ends (ACP's turn usage, which agents report as running session totals, so each turn adds the difference).
  A turn whose agent reports none still counts its time.
  Writes are serialized, so agents finishing together never lose a turn.
- **The comparison** (usage modal, Code graph): for each side, its turns, tokens per turn, time per turn, total tokens and total time.
  Once each side has at least 10 turns, the modal shows the percentage change from graph off to graph on beside each per-turn average, with a sentence explaining the change.
  Before that it says how many turns each side still needs.
  **Reset counts** clears both sides, tool calls and workspace usage, and starts counting again.
- **Reading it.**
  If turns with the graph use no fewer tokens and no less time, the graph does not pay for itself and the default turns off.
  Lookup accuracy (did the graph return the right record), record integrity (did the record match the source) and task outcome stay separate measures.

**Velocity's own uses** (later, built on the graph):
- **AIRC overlaps by symbol** (F14): two agents editing different functions in one file don't overlap.
  An agent changing a symbol another agent's claimed work calls is told.
- **Swarm split** (F10, F16): work items are grouped by the subsystems their likely code falls in, so concurrent agents collide less.
- **PR duty** (F7): a review starts from the change's impact set: the changed symbols, and what depends on them.

**Accept when:**
- [ ] Every new agent of every provider has the graph tools with no per-provider setup.
- [ ] A name defined more than once returns `ambiguous` with every candidate, never one of them.
- [ ] After an agent edits a file, the next query answers from the edited file, never from an older parse.
- [ ] After a merge to the base, only the files it changed are parsed.
- [ ] A query answers in under 50 ms on a 100,000-line project.
- [ ] The usage modal reports the active project's graph health, base freshness, coverage, cache and tool usage, and can retry a failed base index.
- [ ] The usage modal compares tokens and time per turn for the active project with the graph on and off, and its switch decides whether new agents get the graph.
