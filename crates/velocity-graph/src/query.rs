//! The answers the graph tools give (F19 "Agent tools"): compact text lines, each record with
//! its address, kind, name, file and lines, and failures that say why.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt::Write;

use crate::graph::{Graph, Provenance, Record, RecordKind, Relation};
use crate::search::Search;

/// Every answer is cut to this many characters (F19 "Budget").
pub const BUDGET: usize = 8_000;
/// Links shown per relation in `graph_read` before the rest are counted.
const LINKS_SHOWN: usize = 12;

/// A lookup by address, path or name (F19 accuracy rules: exact or a classified failure).
pub enum Lookup {
    Found(usize),
    Ambiguous(Vec<usize>),
    /// Nothing has this name; the nearest names are given.
    Missing(Vec<usize>),
    /// An address no record has any more: the code it named changed.
    Stale,
}

pub fn lookup(graph: &Graph, search: &Search, target: &str) -> Lookup {
    let target = target.trim().trim_matches('`');
    if is_address(target) {
        return graph
            .by_address
            .get(target)
            .map_or(Lookup::Stale, |found| Lookup::Found(*found));
    }
    let path = target.trim_start_matches("./").replace('\\', "/");
    if let Some(found) = graph.by_path.get(&path) {
        return Lookup::Found(*found);
    }
    let named = graph.named(target);
    match named.len() {
        1 => return Lookup::Found(named[0]),
        count if count > 1 => return Lookup::Ambiguous(named.to_vec()),
        _ => {}
    }
    let suffix = format!("/{path}");
    let by_suffix: Vec<usize> = graph
        .by_path
        .iter()
        .filter(|(candidate, _)| candidate.ends_with(&suffix))
        .map(|(_, record)| *record)
        .collect();
    match by_suffix.len() {
        1 => Lookup::Found(by_suffix[0]),
        0 => Lookup::Missing(
            search
                .find(graph, target, 5, |_| true)
                .into_iter()
                .map(|hit| hit.record)
                .collect(),
        ),
        _ => Lookup::Ambiguous(by_suffix),
    }
}

pub fn is_address(text: &str) -> bool {
    let mut parts = text.splitn(2, '.');
    matches!(parts.next(), Some("1" | "2" | "3"))
        && parts.next().is_some_and(|code| {
            code.len() >= 5
                && code
                    .chars()
                    .all(|character| character.is_ascii_digit() || character.is_ascii_lowercase())
        })
}

/// The failure text for a lookup that found no single record.
pub fn failure(graph: &Graph, target: &str, lookup: &Lookup) -> String {
    let mut out = String::new();
    match lookup {
        Lookup::Found(_) => {}
        Lookup::Ambiguous(candidates) => {
            let _ = writeln!(
                out,
                "ambiguous: `{target}` names {} records. Pass one address:",
                candidates.len()
            );
            for candidate in candidates.iter().take(30) {
                let _ = writeln!(out, "{}", line(graph, *candidate));
            }
        }
        Lookup::Missing(nearest) => {
            let nearest_label = if nearest.is_empty() {
                " Nothing is close."
            } else {
                " Nearest:"
            };
            let _ = writeln!(out, "missing: nothing is named `{target}`.{nearest_label}");
            for candidate in nearest {
                let _ = writeln!(out, "{}", line(graph, *candidate));
            }
        }
        Lookup::Stale => {
            let _ = writeln!(
                out,
                "stale: no record has address {target} now; the code it named was renamed, moved or deleted. Find it again with graph_find."
            );
        }
    }
    out
}

/// One record on one line: `1.k3f9q method Channel::open  crates/x/src/channel.rs:40-62  pub fn open(…)`.
pub fn line(graph: &Graph, index: usize) -> String {
    let record = &graph.records[index];
    match record.kind {
        RecordKind::Symbol(_) => format!(
            "{} {} {}  {}:{}-{}  {}",
            record.address,
            record.kind.label(),
            record.qualified,
            record.path,
            record.start_line,
            record.end_line,
            record.signature
        ),
        RecordKind::File => format!(
            "{} file {}  ({} symbols)",
            record.address,
            record.path,
            record.down.len()
        ),
        RecordKind::Subsystem => format!(
            "{} subsystem {}  {}/  ({} files)",
            record.address,
            record.name,
            record.path,
            record.down.len()
        ),
    }
}

/// `graph_read`: one record with its chain up, its children and its links.
pub fn read(graph: &Graph, index: usize) -> String {
    let record = &graph.records[index];
    let mut out = line(graph, index);
    out.push('\n');
    if !record.doc.is_empty() {
        let _ = writeln!(out, "  doc: {}", record.doc);
    }
    let mut chain = Vec::new();
    let mut up = record.up;
    while let Some(parent) = up {
        chain.push(format!(
            "{} {} {}",
            graph.records[parent].address,
            graph.records[parent].kind.label(),
            graph.records[parent].name
        ));
        up = graph.records[parent].up;
    }
    if !chain.is_empty() {
        let _ = writeln!(out, "  up: {}", chain.join(" < "));
    }
    if !record.down.is_empty() {
        let names: Vec<String> = record
            .down
            .iter()
            .take(40)
            .map(|child| {
                format!(
                    "{} {}",
                    graph.records[*child].address, graph.records[*child].qualified
                )
            })
            .collect();
        let more = record.down.len().saturating_sub(40);
        let _ = writeln!(
            out,
            "  down ({}): {}{}",
            record.down.len(),
            names.join(", "),
            if more > 0 {
                format!(", +{more} more")
            } else {
                String::new()
            }
        );
    }
    out.push_str(&links(graph, index, true));
    out.push_str(&links(graph, index, false));
    out
}

fn links(graph: &Graph, index: usize, outgoing: bool) -> String {
    let edges = if outgoing {
        &graph.outgoing[index]
    } else {
        &graph.incoming[index]
    };
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for edge in edges {
        groups
            .entry(graph.edges[*edge].relation.label())
            .or_default()
            .push(*edge);
    }
    let mut out = String::new();
    for (relation, edges) in groups {
        let heading = if outgoing {
            relation.to_owned()
        } else {
            format!("{relation} (in)")
        };
        let _ = writeln!(out, "  {heading} ({}):", edges.len());
        let mut sorted = edges.clone();
        sorted.sort_by_key(|edge| provenance_rank(graph.edges[*edge].provenance));
        for edge in sorted.iter().take(LINKS_SHOWN) {
            let edge = &graph.edges[*edge];
            let other = if outgoing { edge.to } else { edge.from };
            let _ = writeln!(
                out,
                "    {} [{}]{}",
                short(graph, other),
                edge.provenance.label(),
                at_line(graph, edge.from, edge.line)
            );
        }
        if sorted.len() > LINKS_SHOWN {
            let direction = if outgoing { "out" } else { "in" };
            let _ = writeln!(
                out,
                "    +{} more: graph_run `AT {}; FOLLOW {relation} {direction}`",
                sorted.len() - LINKS_SHOWN,
                graph.records[index].address
            );
        }
    }
    out
}

fn provenance_rank(provenance: Provenance) -> u8 {
    match provenance {
        Provenance::Extracted => 0,
        Provenance::Inferred => 1,
        Provenance::Ambiguous(_) => 2,
    }
}

fn at_line(graph: &Graph, from: usize, line: u32) -> String {
    if line == 0 {
        return String::new();
    }
    let file = graph.records[from]
        .path
        .rsplit('/')
        .next()
        .unwrap_or_default();
    format!(" at {file}:{line}")
}

/// A record in a link list: address, kind, name and where it is.
pub fn short(graph: &Graph, index: usize) -> String {
    let record: &Record = &graph.records[index];
    match record.kind {
        RecordKind::Symbol(_) => format!(
            "{} {} {}  {}:{}",
            record.address,
            record.kind.label(),
            record.qualified,
            record.path,
            record.start_line
        ),
        _ => format!("{} {} {}", record.address, record.kind.label(), record.path),
    }
}

/// `graph_impact`: what depends on `targets`, transitively, split into links found in the
/// source and leads to check.
pub fn impact(graph: &Graph, targets: &[usize], depth_limit: usize) -> String {
    let mut seeds: Vec<usize> = targets.to_vec();
    for target in targets {
        if graph.records[*target].kind == RecordKind::File {
            seeds.extend(&graph.records[*target].down);
        }
    }
    // (record, depth, every hop extracted)
    let mut found: BTreeMap<usize, (usize, bool, Relation)> = BTreeMap::new();
    let mut queue: VecDeque<(usize, usize, bool)> =
        seeds.iter().map(|seed| (*seed, 0, true)).collect();
    let mut seen: HashSet<usize> = seeds.iter().copied().collect();
    while let Some((record, depth, sure)) = queue.pop_front() {
        if depth >= depth_limit {
            continue;
        }
        for edge in &graph.incoming[record] {
            let edge = &graph.edges[*edge];
            if edge.relation == Relation::Depends {
                continue;
            }
            let sure = sure && edge.provenance == Provenance::Extracted;
            if seen.insert(edge.from) {
                found.insert(edge.from, (depth + 1, sure, edge.relation));
                queue.push_back((edge.from, depth + 1, sure));
            }
        }
    }
    render_impact(graph, targets, &found)
}

fn render_impact(
    graph: &Graph,
    targets: &[usize],
    found: &BTreeMap<usize, (usize, bool, Relation)>,
) -> String {
    let mut out = String::new();
    let names: Vec<String> = targets.iter().map(|target| short(graph, *target)).collect();
    let _ = writeln!(
        out,
        "impact of {}: {} dependents",
        names.join(", "),
        found.len()
    );
    for (title, sure) in [
        ("found in the source (every hop extracted)", true),
        ("leads to check (an inferred or ambiguous hop)", false),
    ] {
        let mut by_subsystem: BTreeMap<&str, Vec<(usize, usize, Relation)>> = BTreeMap::new();
        for (record, (depth, is_sure, relation)) in found {
            if *is_sure == sure {
                let subsystem = &graph.records[graph.records[*record].subsystem].name;
                by_subsystem
                    .entry(subsystem)
                    .or_default()
                    .push((*record, *depth, *relation));
            }
        }
        let count: usize = by_subsystem.values().map(Vec::len).sum();
        let _ = writeln!(out, "{title}: {count}");
        for (subsystem, mut records) in by_subsystem {
            records.sort_by_key(|(record, depth, _)| (*depth, graph.records[*record].path.clone()));
            let _ = writeln!(out, "  {subsystem} ({})", records.len());
            for (record, depth, relation) in records {
                let _ = writeln!(
                    out,
                    "    d{depth} {} ({})",
                    short(graph, record),
                    relation.label()
                );
            }
        }
    }
    out
}

/// Cuts `text` to the budget at a line end, saying how much was cut.
pub fn within_budget(text: String) -> String {
    if text.len() <= BUDGET {
        return text;
    }
    let mut cut = 0;
    for (index, _) in text.match_indices('\n') {
        if index > BUDGET - 200 {
            break;
        }
        cut = index;
    }
    let dropped = text[cut..]
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    format!(
        "{}\n… {dropped} more lines cut to fit 8,000 characters. Narrow the query: a FILTER, a lower depth, or one address.\n",
        &text[..cut]
    )
}
