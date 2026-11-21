//! Spec section aliases, compact split-spec outline, and live §7 item status (#538).

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::path::Path;

use crate::SymbolKind;
use crate::graph::{Graph, Record, RecordKind};
use crate::query::{Lookup, failure, lookup};
use crate::search::Search;

/// One active claim from Rosen or a local AIRC channel, used to annotate §7 items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveClaim {
    pub piece: String,
    pub task: String,
    pub nick: String,
}

/// Whether `path` belongs to the project's spec (`SPEC.md` or Markdown under `spec/`).
pub fn is_spec_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower == "spec.md"
        || (lower.starts_with("spec/") && (lower.ends_with(".md") || lower.ends_with(".markdown")))
}

/// Section number and feature ID aliases for a spec heading, such as `§7`/`7`, `§3.1`/`3.1`,
/// `F16`, or compound split-spec children like `§3.1/F9`.
pub fn heading_aliases(path: &str, title: &str) -> Vec<String> {
    if !is_spec_path(path) {
        return Vec::new();
    }
    let Some(primary) = velocity_spec::section_id(title) else {
        return Vec::new();
    };
    let trimmed = title.trim_start();
    if trimmed.starts_with('§')
        && let Some((_, rest)) = trimmed.split_once('·')
        && let Some(child) = velocity_spec::section_id(rest)
    {
        let parent_plain = primary.trim_start_matches('§');
        let child_plain = child.trim_start_matches('§');
        return vec![
            format!("{primary}/{child_plain}"),
            format!("{parent_plain}/{child_plain}"),
        ];
    }
    let mut aliases = vec![primary.clone()];
    if let Some(plain) = primary.strip_prefix('§') {
        aliases.push(plain.to_owned());
    }
    aliases
}

/// The preferred display alias for a heading (`§7`, `§3.1`, `F16`, `§3.1/F9`).
pub fn primary_alias(record: &Record) -> Option<&str> {
    record.aliases.first().map(String::as_str)
}

/// Whether a search query names a spec section, feature ID, or milestone (so docs aren't
/// downranked by `graph_find`).
pub fn looks_like_spec_query(query: &str) -> bool {
    query.split_whitespace().any(|raw| {
        let token = raw.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '§' | '.' | '/')
        });
        if token.starts_with('§') {
            return true;
        }
        if let Some(id) = velocity_spec::section_id(token) {
            return id.starts_with('§')
                || (token.len() >= 2
                    && token.as_bytes().first().is_some_and(|byte| {
                        matches!(byte, b'F' | b'f' | b'M' | b'm' | b'R' | b'r')
                    }));
        }
        false
    })
}

/// Finds the holder nick if any active AIRC/Rosen claim matches this §7 item.
pub fn claim_holder<'a>(record: &Record, claims: &'a [LiveClaim]) -> Option<&'a str> {
    if record.kind != RecordKind::Symbol(SymbolKind::Item) || claims.is_empty() {
        return None;
    }
    let title = normalize_text(&record.name);
    let container = record.container.as_deref().map(normalize_text);
    for claim in claims {
        if claim_matches_item(claim, &title, container.as_deref()) {
            return Some(claim.nick.as_str());
        }
    }
    None
}

fn claim_matches_item(claim: &LiveClaim, title: &str, container: Option<&str>) -> bool {
    if normalize_text(&claim.task) == title {
        return true;
    }
    let piece = claim.piece.trim();
    let rest = piece
        .strip_prefix("spec:")
        .or_else(|| piece.strip_prefix("SPEC:"))
        .unwrap_or(piece);
    let normalized = normalize_text(rest);
    if normalized == title {
        return true;
    }
    if let Some((group, item)) = normalized.split_once('/')
        && item.trim() == title
    {
        return container.is_none_or(|expected| {
            let group = group.trim();
            group == expected || group.starts_with(expected) || expected.starts_with(group)
        });
    }
    false
}

fn normalize_text(text: &str) -> String {
    text.replace('*', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Compact status badge for a §7 checklist item.
pub fn item_status(record: &Record, claims: &[LiveClaim]) -> String {
    let holder = claim_holder(record, claims);
    match (record.done.unwrap_or(false), holder) {
        (true, Some(nick)) => format!("[done, claimed by {nick}]"),
        (true, None) => "[done]".to_owned(),
        (false, Some(nick)) => format!("[open, claimed by {nick}]"),
        (false, None) => "[open]".to_owned(),
    }
}

/// Counts `(total, open, claimed_open, done)` for a §7 heading's item children.
pub fn plan_counts(
    graph: &Graph,
    heading: &Record,
    claims: &[LiveClaim],
) -> (usize, usize, usize, usize) {
    let mut total = 0;
    let mut open = 0;
    let mut claimed = 0;
    let mut done = 0;
    for &child in &heading.down {
        let item = &graph.records[child];
        if item.kind != RecordKind::Symbol(SymbolKind::Item) {
            continue;
        }
        total += 1;
        if item.done.unwrap_or(false) {
            done += 1;
        } else {
            open += 1;
            if claim_holder(item, claims).is_some() {
                claimed += 1;
            }
        }
    }
    (total, open, claimed, done)
}

/// Options controlling how much of the split spec tree [`outline`] expands.
pub struct OutlineOptions<'a> {
    pub section: Option<&'a str>,
    pub depth: Option<u8>,
    pub open_only: bool,
}

/// Renders the split spec's heading tree with addresses and aliases in one compact call.
pub fn outline(
    graph: &Graph,
    search: &Search,
    root_spec: Option<&str>,
    options: &OutlineOptions<'_>,
    claims: &[LiveClaim],
) -> Result<String, String> {
    let files = ordered_spec_files(graph, root_spec);
    if files.is_empty() {
        return Ok("No SPEC.md or spec/ files are indexed in this workspace.".to_owned());
    }
    if let Some(target) = options
        .section
        .map(str::trim)
        .filter(|target| !target.is_empty())
    {
        return section_outline(graph, search, &files, target, options, claims);
    }
    let headings = collect_headings(graph, &files);
    let max_depth = options
        .depth
        .map(|limit| limit.clamp(1, 6))
        .unwrap_or_else(|| if headings.len() <= 60 { 6 } else { 3 });
    let mut out = String::new();
    let folded_hint = if max_depth < 6
        && headings
            .iter()
            .any(|&index| graph.records[index].level > max_depth)
    {
        format!("; showing depth <= {max_depth}; pass section or depth to expand")
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "Spec outline ({} headings across {} files{folded_hint}):",
        headings.len(),
        files.len()
    );
    let directory_counts = directory_subsection_counts(graph, &files);
    for (position, &index) in headings.iter().enumerate() {
        if graph.records[index].level > max_depth {
            continue;
        }
        let folded = folded_children(graph, &headings, position, max_depth, &directory_counts);
        write_heading_line(&mut out, graph, index, folded, claims);
    }
    Ok(out)
}

fn section_outline(
    graph: &Graph,
    search: &Search,
    files: &[usize],
    target: &str,
    options: &OutlineOptions<'_>,
    claims: &[LiveClaim],
) -> Result<String, String> {
    let resolved = match lookup(graph, search, target) {
        Lookup::Found(index) => index,
        other => return Err(failure(graph, target, &other)),
    };
    let record = &graph.records[resolved];
    if record.kind == RecordKind::Symbol(SymbolKind::Heading) && has_plan_items(graph, record) {
        return Ok(render_plan_section(
            graph,
            resolved,
            options.open_only,
            claims,
        ));
    }
    let directory = readme_directory(&record.path);
    let max_depth = options.depth.map(|limit| limit.clamp(1, 6)).unwrap_or(6);
    let mut out = String::new();
    write_heading_line(&mut out, graph, resolved, 0, claims);
    if let Some(directory) = directory {
        let prefix = format!("{directory}/");
        for &file_index in files {
            let file = &graph.records[file_index];
            if file.path == record.path || !file.path.starts_with(&prefix) {
                continue;
            }
            for &child in &file.down {
                let heading = &graph.records[child];
                if heading.kind == RecordKind::Symbol(SymbolKind::Heading)
                    && heading.level <= max_depth
                {
                    write_heading_line(&mut out, graph, child, 0, claims);
                }
            }
        }
        return Ok(out);
    }
    if let Some(file_index) = record.up {
        for &child in &graph.records[file_index].down {
            let heading = &graph.records[child];
            if child != resolved
                && heading.kind == RecordKind::Symbol(SymbolKind::Heading)
                && heading.start_byte >= record.start_byte
                && heading.end_byte <= record.end_byte
                && heading.level <= max_depth
            {
                write_heading_line(&mut out, graph, child, 0, claims);
            }
        }
    }
    Ok(out)
}

fn render_plan_section(
    graph: &Graph,
    heading_index: usize,
    open_only: bool,
    claims: &[LiveClaim],
) -> String {
    let heading = &graph.records[heading_index];
    let (total, open, claimed, done) = plan_counts(graph, heading, claims);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {}  {}:{}-{}  ({total} items: {open} open, {claimed} claimed, {done} done)",
        heading.address, heading.qualified, heading.path, heading.start_line, heading.end_line
    );
    let mut current_group: Option<&str> = None;
    let mut shown = 0usize;
    for &child in &heading.down {
        let item = &graph.records[child];
        if item.kind != RecordKind::Symbol(SymbolKind::Item) {
            continue;
        }
        if open_only && item.done.unwrap_or(false) {
            continue;
        }
        let group = item.container.as_deref().unwrap_or("");
        if current_group != Some(group) && !group.is_empty() {
            let _ = writeln!(out, "  {group}");
            current_group = Some(group);
        }
        let _ = writeln!(
            out,
            "    {} L{} {}  {}",
            item.address,
            item.start_line,
            item_status(item, claims),
            item.name
        );
        shown += 1;
    }
    if shown == 0 && open_only {
        let _ = writeln!(out, "  (no open items)");
    }
    out
}

fn has_plan_items(graph: &Graph, heading: &Record) -> bool {
    heading
        .down
        .iter()
        .any(|&child| graph.records[child].kind == RecordKind::Symbol(SymbolKind::Item))
}

fn write_heading_line(
    out: &mut String,
    graph: &Graph,
    index: usize,
    folded: usize,
    claims: &[LiveClaim],
) {
    let record = &graph.records[index];
    let indent = "  ".repeat(record.level.saturating_sub(1) as usize);
    let alias = primary_alias(record)
        .filter(|alias| !record.name.starts_with(alias))
        .map(|alias| format!("[{alias}] "))
        .unwrap_or_default();
    let suffix = if has_plan_items(graph, record) {
        let (total, open, claimed, done) = plan_counts(graph, record, claims);
        format!("  ({total} items: {open} open, {claimed} claimed, {done} done)")
    } else if folded > 0 {
        format!("  (+{folded} sub-sections)")
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "{indent}{} {alias}{}  {}:{}-{}{suffix}",
        record.address, record.name, record.path, record.start_line, record.end_line
    );
}

fn ordered_spec_files(graph: &Graph, root_spec: Option<&str>) -> Vec<usize> {
    let by_path: BTreeMap<&str, usize> = graph
        .by_path
        .iter()
        .filter(|(path, _)| is_spec_path(path))
        .map(|(path, &index)| (path.as_str(), index))
        .collect();
    if by_path.is_empty() {
        return Vec::new();
    }
    let mut ordered = Vec::with_capacity(by_path.len());
    let mut seen = HashSet::new();
    if let Some((&root_path, &root_index)) = by_path
        .iter()
        .find(|(path, _)| path.eq_ignore_ascii_case("spec.md"))
    {
        ordered.push(root_index);
        seen.insert(root_path);
    }
    if let Some(markdown) = root_spec {
        for link in spec_index_links(markdown) {
            if let Some((&path, &index)) = by_path.get_key_value(link.as_str())
                && seen.insert(path)
            {
                ordered.push(index);
            }
        }
    }
    let mut remaining: Vec<(&str, usize)> = by_path
        .iter()
        .filter(|(path, _)| !seen.contains(**path))
        .map(|(&path, &index)| (path, index))
        .collect();
    remaining.sort_by(|(left, _), (right, _)| spec_path_key(left).cmp(&spec_path_key(right)));
    ordered.extend(remaining.into_iter().map(|(_, index)| index));
    ordered
}

fn spec_path_key(path: &str) -> (String, u8, String) {
    let (directory, file) = path.rsplit_once('/').unwrap_or(("", path));
    let readme_first = u8::from(!file.eq_ignore_ascii_case("readme.md"));
    (directory.to_owned(), readme_first, file.to_owned())
}

fn spec_index_links(markdown: &str) -> Vec<String> {
    let mut links = Vec::new();
    for line in markdown.lines() {
        let mut rest = line;
        while let Some(open) = rest.find("](spec/") {
            let after = &rest[open + 2..];
            let Some(close) = after.find(')') else { break };
            let target = after[..close].trim().replace('\\', "/");
            if !target.is_empty() {
                links.push(target);
            }
            rest = &after[close + 1..];
        }
    }
    links
}

fn collect_headings(graph: &Graph, files: &[usize]) -> Vec<usize> {
    let mut headings = Vec::new();
    for &file_index in files {
        let mut symbols = graph.records[file_index].down.clone();
        symbols.sort_by_key(|&index| graph.records[index].start_byte);
        for symbol in symbols {
            let record = &graph.records[symbol];
            if record.kind == RecordKind::Symbol(SymbolKind::Heading)
                && record.name != "Spec file index"
            {
                headings.push(symbol);
            }
        }
    }
    headings
}

fn directory_subsection_counts(graph: &Graph, files: &[usize]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for &file_index in files {
        let path = &graph.records[file_index].path;
        let Some((directory, file_name)) = path.rsplit_once('/') else {
            continue;
        };
        if !directory.starts_with("spec/") || file_name.eq_ignore_ascii_case("readme.md") {
            continue;
        }
        let heading_count = graph.records[file_index]
            .down
            .iter()
            .filter(|&&index| graph.records[index].kind == RecordKind::Symbol(SymbolKind::Heading))
            .count();
        *counts.entry(directory.to_owned()).or_default() += heading_count;
    }
    counts
}

fn folded_children(
    graph: &Graph,
    headings: &[usize],
    position: usize,
    max_depth: u8,
    directory_counts: &BTreeMap<String, usize>,
) -> usize {
    let current = &graph.records[headings[position]];
    if current.level < max_depth {
        return 0;
    }
    if let Some(directory) = readme_directory(&current.path)
        && let Some(&count) = directory_counts.get(directory)
    {
        return count;
    }
    headings[position + 1..]
        .iter()
        .take_while(|&&later| {
            let next = &graph.records[later];
            next.path == current.path && next.level > current.level
        })
        .count()
}

fn readme_directory(path: &str) -> Option<&str> {
    let (directory, file_name) = path.rsplit_once('/')?;
    (directory.starts_with("spec/") && file_name.eq_ignore_ascii_case("readme.md"))
        .then_some(directory)
}

/// Reads active claims from a legacy local AIRC channel folder (`<app-data>/airc/<project>`).
pub fn read_local_airc_claims(cache_folder: &Path) -> Vec<LiveClaim> {
    let Some(project) = cache_folder.file_name() else {
        return Vec::new();
    };
    let Some(app_data) = cache_folder.parent().and_then(Path::parent) else {
        return Vec::new();
    };
    let channel_dir = app_data.join("airc").join(project);
    let Ok(entries) = std::fs::read_dir(&channel_dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect();
    files.sort();
    let mut held: BTreeMap<String, LiveClaim> = BTreeMap::new();
    for path in files {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines() {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(claim) = entry.get("claim").filter(|value| value.is_object()) else {
                continue;
            };
            let piece = claim["piece"].as_str().unwrap_or_default();
            let task = claim["task"].as_str().unwrap_or_default();
            let nick = claim["nick"].as_str().unwrap_or_default();
            let key = if piece.is_empty() { task } else { piece };
            if key.is_empty() {
                continue;
            }
            match entry["kind"].as_str() {
                Some("claim") => {
                    held.insert(
                        key.to_owned(),
                        LiveClaim {
                            piece: piece.to_owned(),
                            task: task.to_owned(),
                            nick: nick.to_owned(),
                        },
                    );
                }
                Some("released") => {
                    held.remove(key);
                }
                _ => {}
            }
        }
    }
    held.into_values().collect()
}
