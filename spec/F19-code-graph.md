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
  A Rosen answer uses the workspace's latest saved snapshot tree at the start of the call, including when returning source lines.
  A git answer rehashes the named files before returning, and rebuilds the answer if they changed.
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
  - Test, example and stress code, including inline Rust `#[cfg(test)]` modules, is never a candidate for production code.
  - In JavaScript and TypeScript, a name links only through a definition in the file or an import.
  - A call inside a macro's arguments is `inferred` at best.
- **No tool is restricted (Rule 1).**
  Agents keep Read, Grep and the shell.
  The tools' instructions say when the graph is the better path (finding a symbol, its callers, what a change affects) and when it isn't (strings, literals, config values, build output).
- **No model in the index.**
  Parsing and resolution are deterministic, so building the graph spends no subscription usage.
  Docs are read for structure only.

**What it holds** (planes, after A.TXY):

| Plane | Records | Links |
|---|---|---|
| 1 · Symbols | functions, methods, types (struct, enum, trait, class, interface), Rust named fields and enum variants, constants, macros, Svelte components, Lean theorems, Markdown headings and §7 checklist items | UP to the file (or the §7 heading for plan items); `calls`, `implements`, `references`; `implements` from a trait method's implementation to its declaration; `calls` from frontend code into a Tauri command (`commands.checkForUpdate`, `invoke("check_for_update")`) or an imported Svelte component; `mentions` from a doc heading to the code its section names in backticks |
| 2 · Files | each tracked source and Markdown file | UP to the subsystem, DOWN to its symbols; `imports` to the files and the symbols it imports (through `export * from` barrels) |
| 3 · Subsystems | workspace members and packages (Cargo members, `package.json` workspaces and packages, Go modules) and otherwise top-level folders | DOWN to their files; `depends` from the manifests |

- **Languages and provenance:**
  - Rust, TypeScript, JavaScript and Svelte have dedicated queries for declarations and explicitly linked calls and imports.
    JavaScript and TypeScript follow relative module and `$lib` imports through star and named re-exports, including aliases.
    Svelte adds its component declaration and simple imported component tags in templates.
  - Lean indexes common named declarations and module imports.
    Elaborated terms, tactic calls and notation have no edges.
  - Python, Go, C#, Java, C and C++ use grammar tags for definitions, and classify receiver calls as leads when the receiver type is unknown.
    Python also records explicit imports.
  - Markdown has headings, classified mentions and per-item `item` records for §7 implementation-order checklists with done and live claimed state (#538).
    Spec headings index their section numbers and feature IDs as aliases (`§7`, `§3.1`, `F16`, `§3.1/F9`), and headings print once per result line without repeating the title as a signature (#538).
    TOML, JSON, YAML, SQL, shell scripts and other unsupported languages have file records only, with supported manifests contributing subsystem dependencies.
  - Receiver calls without type evidence, macro argument calls and non-unique name matches carry their existing classified provenance.
    Dynamic JS and Svelte component expressions and Rust field accesses requiring type inference have no confirmed edges.
- **Left out:** generated files such as lockfiles and generated bindings.
  Rosen uses its snapshot's tracked paths, while git combines tracked files and non-ignored additions.
- **Addresses:** each record has a short address such as `1.k3f9q`: its plane, then base-36 characters from a hash of its path, qualified name and kind.
  An address stays the same while the symbol keeps its name and file, so an agent can pass it back in later calls.
  A moved or renamed symbol returns `stale` with the new address when the previous workspace view is available and a refresh finds a unique matching span or same-file signature.

**Building and keeping it current:**
- **Parse cache.**
  Velocity keeps one parse cache per project in the app data folder: each file's symbols and references, keyed by blob ID, language, parser version and Velocity version.
  It is an append-only log of checksummed records shared by every agent of the project, so cached parses can be reused across workspaces and commits.
  A torn or corrupt record at the end of the log is dropped and that file is parsed again.
  The reader streams records and retains up to 64 MiB of recent serialized parses.
  Compaction keeps those recent records and rewrites the log before it grows beyond twice the cache budget.
  Cache misses are resolved under the project's build lock, with a second cache check before parsing.
- **Shared project graph.**
  The MCP connections forward workspace queries to one project worker with one live graph and search index.
  Continuous dispatch and discovery reporting use the same worker to resolve code references in issues against the project base.
  Workspaces at the same Rosen tree reuse the index, and switching to a different tree applies file changes to the current project view.
  On Rosen, each tool call checks the workspace marker Rosen advances when it stores a snapshot.
  The marker also wakes the background indexer so Usage can show workspaces catching up.
  The graph compares tree IDs, skips identical subtrees, and obtains only changed blobs that miss the cache or supply manifest data.
  A saved edit reaches lookup and impact in the 2 s snapshot window, and graph source reads use the same immutable tree.
  On git, a worktree watcher batches edits in the background, tool calls refresh pending changes, and the span check catches any missed edits.
  Switching git workspaces lists and rehashes the new workspace so unchanged content can reuse the index.
- **Warming.**
  Opening the Rosen project event stream warms the base, then lands and reverts wake the same background worker to update its tree and parse changed cache misses.
  A workspace starting at a warmed base reuses its index and any retained parses of unchanged blobs.
  Git warms after base merges on the trigger that catches up the ready pool (F5).
- **Speed:** a query answers in under 50 ms on a 100,000-line project.
  Building the graph for such a project takes under 10 s with an empty cache and under 1 s with a full one.

**Standalone indexer (C11, #428).**
Collapse serves the graph for landed commits by running Velocity's indexer as the `velocity-graph` binary, pinned like `git`.
It is a bin target of the `velocity-graph` crate, with no Tauri or UI dependencies.
Each release publishes a Linux x86-64 build as `velocity-graph-<version>-x86_64-unknown-linux-gnu.tar.gz` with its SHA-256 checksum in `<archive>.sha256`.
The release workflow publishes both with the release it builds.
Publishing a release built any other way, such as a local build, starts the `Graph indexer` workflow, which attaches both; dispatching it with a tag adds them to an earlier release (#518).
An archive and checksum already on a release are never replaced, because Collapse pins the checksum.
It indexes one commit of a git repository, bare or not, reading its regular files from git objects with no checkout.
Git blob IDs are the file hashes and parse cache keys, so a commit gives the same records and addresses as the MCP tools for the same tree.
The parse cache lives in the folder given with `--cache`, so only blobs missing from it are read and parsed.
The tools skip the staleness check because a commit never changes.

| Command | Does |
|---|---|
| `velocity-graph version` | Prints the schema version, the indexer's release version, the parser version and the tools it answers. |
| `velocity-graph index --repository <path> --commit <revision> --cache <folder>` | Indexes the commit and prints its full ID, file, symbol and subsystem counts, parses made and reused, time, and skipped files by reason. |
| `velocity-graph query --repository <path> --commit <revision> --cache <folder> <tool> [<json-arguments>]` | Answers `graph_find`, `graph_read`, `graph_spec`, `graph_impact` or `graph_run` with the MCP tool's arguments and text. |
| `velocity-graph serve --repository <path> --cache <folder>` | Reads one JSON request per line, `{id?, commit, tool?, arguments?}`, and answers each on one line with one warm index, re-indexing only files whose blobs changed between commits. |

Every answer is one JSON object carrying `schema` (currently 1), which changes only when a field changes meaning or goes away.
A tool answer has `commit`, `tool`, `ok` and `text`, and `ok: false` marks a classified failure such as `ambiguous` or `missing`.
A fault is `{schema, error}`, exiting 1, and a usage error exits 2.

**Backend measurements (#442).**
Measured on Windows 11 with a Core Ultra 7 265F using the debug `velocity-graph` example `graph_stats`, a local Rosen server and a new project cache.
Startup includes opening the graph and building its search index, and peak working set refers to the example process.
Velocity used its #442 workspace, and Rosen used the pinned `63220cb` source imported into an isolated local server with its warm-pool configuration omitted.
Rosen's warm measurement opened a second workspace at the same base.
With the workspace watcher running, a live Rust edit took 330 ms to reach the snapshot, 585 ms to appear in `graph_find`, and 586 ms to appear in `graph_impact`, measured from the write.
An edit during standalone watcher initialization took 3.8 s to reach its first snapshot, with the graph answering 1.05 s after that snapshot.

| Project | Indexed files | Cold startup | Warm startup | Warm parses | Parse cache | Peak working set, cold / warm | Warm find |
|---|---:|---:|---:|---:|---:|---:|---:|
| Velocity | 966 | 1,950 ms | 749 ms | 0 | 10.39 MiB | 75.2 / 64.0 MiB | 33 ms |
| Rosen | 534 | 4,157 ms | 608 ms | 0 | 6.68 MiB | 51.5 / 44.0 MiB | 25 ms |

For these two reference projects, budget 10 s cold startup, 1 s warm startup, 50 ms for a warm lookup, 16 MiB for a freshly populated project cache and 96 MiB peak working set per graph process.
Recheck with `rosen run -- cargo run -p velocity-graph --example graph_stats -- <workspace> <cache-folder>` on the first workspace and then on a second workspace with the same base and cache folder.

**Agent tools.**
Every session Velocity starts, on every provider and in every role, gets an MCP server named `velocity-graph` in `session/new`, beside `airc` (§3.2, F14).
The name carries Velocity's reserved prefix, so it never clashes with a server of your own, such as Graphify's.
It runs Velocity's own executable as `velocity --graph-mcp <app-data> <project-id> <workspace> [thread]`.
The discovery and search tools advertise `anthropic/alwaysLoad` in `tools/list` so compatible Claude Code versions (2.1.325 and later) keep them in context.

| Tool | Does |
|---|---|
| `graph_find { query, plane?, kind?, limit? }` | Ranks candidates for a name, spec alias or plain-language description. Matching is by words, with camelCase and snake_case split, intact section references (`§7`, `§3.1`), word stems and rarer words weighted higher. A record's score scales with the square of the share of the query's words it matches. An exact name or alias ranks first, then names that start with the query. Tests, examples and docs rank lower unless the query asks for them. Returns addresses with kind, name, file:line and signature (printing Markdown headings once and annotating §7 items with `[done]`, `[open]` or `[open, claimed by <nick>]`). A result that is the only exact match for the name or alias is marked `exact`. |
| `graph_read { target, source? }` | One record by address, exact name, spec alias (`§7`, `§3.1`, `F16`) or path. A file gives an outline of its symbols in source order with their addresses and line ranges. A §7 heading summarizes its open, claimed and done items with addresses. A symbol gives its signature, doc summary, location and links grouped by relation with their counts. `source: true` adds the record's source lines. Fails as the accuracy rules say. |
| `graph_spec { section?, depth?, open_only? }` | Returns the split spec's heading tree with addresses, section aliases and folded subsection or §7 item counts in one compact call. Passing `section` (such as `§7`, `§3.1` or `F5`) or `depth` expands that subtree; `open_only` filters §7 to open items. |
| `graph_impact { targets, depth? }` | What depends on symbols or files, followed transitively through calls, implementations, type uses, imports and doc mentions (2 hops by default, at most 6). A type brings its methods and a file its symbols, and an implementation of a trait method brings the callers of the trait method. A hub (a record with at least the 99th percentile of incoming links, and at least 50) is listed but not followed. `ambiguous` and `unconfirmed` links are listed only as direct leads. The answer is grouped by subsystem with depth, and links found in the source are listed apart from leads to check. It says that no dependents doesn't prove code unused. |
| `graph_overlaps { paths? }` | Live Rosen overlap paths, edited symbols on each side, and the names both changes edited. Omitting paths checks the workspace's own claims and edits. |
| `graph_run { program }` | A pointer program of up to 16 steps, run in one call over one register: `FIND`, `AT`, `READ`, `UP`, `DOWN`, `FOLLOW <relation> [in\|out] [all]`, `FILTER <kind\|plane\|path glob\|name>`, `PATH <target>`, `COUNT`, `SOURCE`. Returns the result and a one-line audit for each step (what it did and how many records it held). `PATH` lists each hop with its relation and provenance, and the path's confidence: that of its weakest hop. It never passes through a hub. For example, `FIND "approval record"; FOLLOW calls in; FILTER path crates/velocity-github/**; COUNT` replaces several searches and reads with one call. |
| `graph_grep { pattern, path?, glob?, case_insensitive?, fixed?, files_only?, context?, limit? }` | Searches workspace files with ripgrep (`--no-require-git`, falling back to an `ignore`-crate walk when `rg` is absent), respecting `.gitignore` and capping output to the budget. |
| `graph_glob { pattern, path?, limit? }` | Lists workspace files matching a glob pattern (including `{a,b}` brace expansion), respecting `.gitignore` and capping output to the budget. |

- **Budget.**
  Every answer is capped at 8,000 characters.
  A cut answer says so on its first line, with how much was kept and how to narrow it.
- **Instructions and session-start map.**
  Shared server guidance and the session-start map (about 250 tokens: the workspace, its file and symbol counts, its biggest subsystems and its most-used symbols with their addresses) are published once in the session preface rather than in MCP `initialize.instructions` or repeated across tool descriptions (#538).
  Guidance is written as facts, not orders: imperative rules read like prompt injection and get ignored.
  A new session with the graph enabled starts its first prompt with that guidance, the workspace map when available within the startup budget, and up to five `graph_find` matches for the task title when one is set.

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
  The modal compares graph calls with raw source reads and searches observed in ACP tool events.
  Its thread rows show input, cached input, output, graph calls, observed raw reads and searches, and the subset of those calls that requested a whole file.
  Graph calls are counted by the MCP server, and Velocity's own startup lookup does not count as an agent call.
  Raw reads and searches require source evidence in the ACP event, such as a file path or source glob, and token totals depend on the usage the provider reports.
- **Totals.**
  Every turn of every agent, in every role, adds its tokens and its working time to the project's totals for the side it ran on: the graph on or off.
  Claude and Codex report usage for the current prompt, so each turn adds the full reported counts.
  Providers that follow ACP's running session totals add the difference since the previous report.
  The meter saves reported thought tokens and uses the adapter's reported total for the comparison.
  A turn whose agent reports none still counts its time.
  Writes are serialized, so agents finishing together never lose a turn.
  Failed turn writes are logged and retried twice.
  Old graph usage totals from before the accounting fix are cleared on upgrade, and each project's graph switch is retained.
- **The comparison** (usage modal, Code graph): for each side, its turns, tokens per turn, time per turn, total tokens and total time.
  Once each side has at least 10 turns, the modal shows the percentage change from graph off to graph on beside each per-turn average, with a sentence explaining the change.
  Before that it says how many turns each side still needs.
  **Reset counts** clears both sides, tool calls and workspace usage, and starts counting again.
- **Reading it.**
  If turns with the graph use no fewer tokens and no less time, the graph does not pay for itself and the default turns off.
  Lookup accuracy (did the graph return the right record), record integrity (did the record match the source) and task outcome stay separate measures.

**Adoption check (#455).**
Ten fresh Claude Code 2.1.326 ACP sessions on read-only code navigation tasks in the Velocity workspace received title matches and made 16 graph calls, 24 observed raw source reads or searches, and no whole-file source reads.
Compared with ten graph-off sessions on the same tasks, estimated carried raw read and search output dropped from 101,411 to 46,229 tokens (54%).
Including graph tool output gives 78,062 tokens (23% lower).
These estimates use the #438 transcript method, weighting each tool result by the number of model calls that could still see it.

**Velocity's own uses:**
- **AIRC and Rosen claims** (F14): overlap notices name the file and, when edits are available, show whether both agents changed the same symbols.
  Symbol details are advisory and existing path claims still govern coordination.
- **Swarm split** (F10, F16): the regular swarm interleaves launches from different likely subsystems, and native goal-swarm selection skips pieces in held subsystems.
- **Review swarm** (F7): review starts with impact from the changed lines in the exact parent and landed trees, including the callers, references and imports of the affected symbols.

**Accept when:**
- [ ] Every new agent of every provider has the graph tools with no per-provider setup.
- [ ] A name defined more than once returns `ambiguous` with every candidate, never one of them.
- [ ] After an agent edits a file, the next query answers from the edited file, never from an older parse.
- [ ] After a merge to the base, only the files it changed are parsed.
- [ ] A query answers in under 50 ms on a 100,000-line project.
- [ ] The usage modal reports the active project's graph health, base freshness, coverage, cache and tool usage, and can retry a failed base index.
- [ ] The usage modal compares tokens and time per turn for the active project with the graph on and off, and its switch decides whether new agents get the graph.
