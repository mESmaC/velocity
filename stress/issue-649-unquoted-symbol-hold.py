"""Trace a false F9 overlap from ordinary title prose into a dispatch hold."""
from pathlib import Path
import re
import sys


def read(path):
    return Path(path).read_text(encoding="utf-8")


overlap = read("src-tauri/src/continuous_dispatch/overlap.rs")
graph = read("crates/velocity-graph/src/graph.rs")
resolver = read("src-tauri/src/graph_access.rs")

recheck = overlap.split("fn recheck(", 1)[1].split("    fn ", 1)[0]
update = overlap.split("fn update(", 1)[1].split("    fn ", 1)[0]
hold = overlap.split("fn hold(", 1)[1].split("    fn ", 1)[0]
lookup = graph.split("fn mention_path(", 1)[1].split("    fn mentioned_file(", 1)[0]

predicted = (
    "tokens.filter_map(|mention| self.mention_path(mention, false))" in graph
    and "!mention.chars().any(char::is_uppercase)" in lookup
    and ".named(mention)" in lookup
    and "paths.all(|other| other == path).then_some(path)" in lookup
    and "velocity_graph::mentioned_paths(" in resolver
)
direct = "issue_paths(self.data, self.project, &descriptions)" in recheck
cached = (
    "self.named_paths(&backlog, &blockers)" in recheck
    and "self.cached_issue_paths(backlog, true)" in overlap
    and "issue_paths(self.data, self.project, &descriptions)" in overlap
)
dispatch = (
    re.search(r"self\.update\(issue, (?:code_paths\()?paths", recheck)
    and "self.hold(number, &paths, blockers, waits, released)" in update
    and "shared_files(paths, peer)" in hold
    and "named.intersection(&blocker.edited)" in overlap
    and "IssueOverlap::new(identity, blocker, Some(thread), shared)" in hold
)

# Explore is a unique symbol in the blocker file. The waiting issue uses the same
# word as a page name and explicitly names only its new, unrelated web page file.
title = "Explore: add public repository cards to web/routes/explore.tsx"
symbol_index = {"explore": "server/internal/query/filters.rs"}
edited = {"server/internal/query/filters.rs"}
if predicted and (direct or cached) and dispatch:
    graph_paths = {
        symbol_index[token.rstrip(".:").lower()]
        for token in title.split()
        if any(character.isupper() for character in token)
        and token.rstrip(".:").lower() in symbol_index
    }
    shared = sorted(graph_paths & edited)
    if shared:
        print(f"FAIL: {title!r} is held on {shared[0]!r} through a bare symbol match; "
              "the waiting issue names a different file.", file=sys.stderr)
        sys.exit(1)
