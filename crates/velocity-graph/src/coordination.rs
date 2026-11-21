//! Graph queries used by claim notices, swarm assignment and post-land review.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rosen_client::Client;
use rosen_client::protocol::{Diff, DiffRequest, LineKind, OverlapRecord, ReadAnswer, ReadRequest};

use crate::extract::{Definition, extract};
use crate::graph::{Graph, RecordKind};
use crate::search::Search;
use crate::{Error, Index, Language};

pub fn git_changes(root: &Path, base: &str, path: &str) -> Vec<String> {
    if path.is_empty()
        || !Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Vec::new();
    }
    let before = crate::files::git_blob(root, base, path);
    let after = std::fs::read_to_string(root.join(path)).ok();
    symbol_changes(path, before.as_deref(), after.as_deref())
}

pub async fn rosen_changes(
    client: &Client,
    revision: &str,
    paths: &[String],
) -> BTreeMap<String, Vec<String>> {
    let request = DiffRequest {
        to: Some(revision.to_owned()),
        paths: paths.to_vec(),
        context: Some(0),
        ..DiffRequest::default()
    };
    let Ok(diff) = client.diff(request).await else {
        return BTreeMap::new();
    };
    let mut result = BTreeMap::new();
    for file in diff
        .files
        .iter()
        .filter(|file| !file.binary && Language::of(&file.path).is_some())
        .take(8)
    {
        let before = if file.old.is_some() {
            snapshot(client, &diff.from.to_string(), &file.path).await
        } else {
            None
        };
        let after = if file.new.is_some() {
            snapshot(client, &diff.to.to_string(), &file.path).await
        } else {
            None
        };
        if file.old.is_some() != before.is_some() || file.new.is_some() != after.is_some() {
            continue;
        }
        let symbols = symbol_changes(&file.path, before.as_deref(), after.as_deref());
        if !symbols.is_empty() {
            result.insert(file.path.clone(), symbols);
        }
    }
    result
}

pub async fn rosen_overlap_symbols(
    client: &Client,
    requester: &str,
    overlap: &OverlapRecord,
) -> serde_json::Value {
    let (ours, theirs) = tokio::join!(rosen_changes(client, requester, &overlap.paths), async {
        rosen_changes(client, &overlap.change.to_string(), &overlap.paths).await
    });
    let symbols: BTreeMap<_, _> = overlap
        .paths
        .iter()
        .filter_map(|path| {
            let ours = ours.get(path).map(Vec::as_slice).unwrap_or_default();
            let theirs = theirs.get(path).map(Vec::as_slice).unwrap_or_default();
            (!ours.is_empty() || !theirs.is_empty()).then(|| {
                (
                    path,
                    serde_json::json!({"requester": ours, "holder": theirs,
                "shared": ours.iter().filter(|name| theirs.contains(name)).collect::<Vec<_>>() }),
                )
            })
        })
        .collect();
    serde_json::json!(symbols)
}

async fn snapshot(client: &Client, revision: &str, path: &str) -> Option<String> {
    let answer = client
        .read_file(ReadRequest {
            at: Some(revision.to_owned()),
            path: path.to_owned(),
        })
        .await
        .ok()?;
    let ReadAnswer::File { data, .. } = answer else {
        return None;
    };
    String::from_utf8(data).ok()
}

fn symbol_changes(path: &str, before: Option<&str>, after: Option<&str>) -> Vec<String> {
    let Some(language) = Language::of(path) else {
        return Vec::new();
    };
    let definitions = |text: Option<&str>| {
        let Some(text) = text.filter(|text| text.len() <= 1024 * 1024) else {
            return BTreeMap::new();
        };
        extract(language, text)
            .definitions
            .into_iter()
            .map(|definition| (qualified(&definition, language), definition.span_hash))
            .collect::<BTreeMap<_, _>>()
    };
    let old = definitions(before);
    let new = definitions(after);
    old.keys()
        .chain(new.keys())
        .filter(|name| old.get(*name) != new.get(*name))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(12)
        .collect()
}

fn qualified(definition: &Definition, language: Language) -> String {
    let separator = if language == Language::Lean {
        "."
    } else {
        "::"
    };
    definition.container.as_ref().map_or_else(
        || definition.name.clone(),
        |container| format!("{container}{separator}{}", definition.name),
    )
}

pub fn review_impact(index: &Index, diff: &Diff, before: bool) -> String {
    let graph = &index.graph;
    let mut targets = BTreeSet::new();
    for file in &diff.files {
        let Some(&record) = graph.by_path.get(&file.path) else {
            continue;
        };
        let mut changed = false;
        for hunk in &file.hunks {
            let mut line = if before {
                hunk.old_start
            } else {
                hunk.new_start
            };
            let (wanted, other) = if before {
                (LineKind::Removed, LineKind::Added)
            } else {
                (LineKind::Added, LineKind::Removed)
            };
            for change in &hunk.lines {
                if change.kind == wanted {
                    targets.insert(symbol_at(graph, record, line));
                    changed = true;
                }
                if change.kind != other {
                    line += 1;
                }
            }
        }
        if !changed && (file.binary || file.hunks.is_empty()) {
            targets.insert(record);
        }
    }
    let targets: Vec<_> = targets.into_iter().collect();
    let mut result = targets
        .iter()
        .take(30)
        .map(|target| crate::query::line(graph, *target))
        .collect::<Vec<_>>()
        .join("\n");
    if !targets.is_empty() {
        result.push_str("\n\n");
        result.push_str(&crate::query::impact(graph, &targets, 2));
    }
    crate::query::within_budget(result)
}

fn symbol_at(graph: &Graph, file: usize, line: u32) -> usize {
    graph.records[file]
        .down
        .iter()
        .copied()
        .filter(|index| {
            let symbol = &graph.records[*index];
            symbol.start_line <= line && line <= symbol.end_line
        })
        .min_by_key(|index| {
            let symbol = &graph.records[*index];
            symbol.end_byte.saturating_sub(symbol.start_byte)
        })
        .unwrap_or(file)
}

pub struct SubsystemMap {
    graph: Graph,
    search: Search,
}

impl SubsystemMap {
    pub fn from_index(index: Index) -> Self {
        let graph = index.graph;
        let search = Search::new(&graph, None);
        Self { graph, search }
    }

    /// The indexed subsystem of an exact repository-relative file path.
    pub fn for_file(&self, path: &str) -> Option<&str> {
        let file = self.graph.records.get(*self.graph.by_path.get(path)?)?;
        let subsystem = &self.graph.records[file.subsystem].path;
        (!subsystem.is_empty()).then_some(subsystem)
    }

    /// A probable subsystem for a title or path, leaving ties unassigned.
    pub fn classify(&self, text: &str) -> Option<String> {
        let graph = &self.graph;
        let mut votes = BTreeMap::<_, usize>::new();
        for (rank, hit) in self
            .search
            .find(graph, text, 8, |record| {
                let record = &graph.records[record];
                !record.test && !matches!(record.kind, RecordKind::Symbol(kind) if kind.is_doc())
            })
            .iter()
            .enumerate()
        {
            let record = &graph.records[hit.record];
            let subsystem = &graph.records[record.subsystem];
            let weight = if record.kind == RecordKind::Subsystem {
                16
            } else {
                8 - rank
            };
            *votes.entry(&subsystem.path).or_default() += weight;
        }
        let mut ranked: Vec<_> = votes.into_iter().collect();
        ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        let (path, score) = ranked.first()?;
        if path.is_empty() || ranked.get(1).is_some_and(|(_, next)| score == next) {
            return None;
        }
        Some((*path).clone())
    }
}

pub fn subsystem_map(
    data: &Path,
    project: u32,
    root: &Path,
    rosen: bool,
) -> Result<SubsystemMap, Error> {
    let cache = data.join("graph").join(project.to_string());
    let index = if rosen {
        let server = velocity_rosen::ProjectServer::new(data, project);
        Index::open_revision(server.endpoint().clone(), "base", &cache)?
    } else {
        Index::open(root, &cache)?
    };
    Ok(SubsystemMap::from_index(index))
}

/// Start work from different subsystems first, preserving order within each group.
pub fn spread<T>(items: Vec<(Option<String>, T)>) -> Vec<(Option<String>, T)> {
    let mut groups: Vec<(Option<String>, std::collections::VecDeque<T>)> = Vec::new();
    for (subsystem, item) in items {
        if let Some((_, group)) = groups.iter_mut().find(|(key, _)| *key == subsystem) {
            group.push_back(item);
        } else {
            groups.push((subsystem, std::collections::VecDeque::from([item])));
        }
    }
    let mut ordered = Vec::new();
    while !groups.is_empty() {
        for (name, group) in &mut groups {
            if let Some(item) = group.pop_front() {
                ordered.push((name.clone(), item));
            }
        }
        groups.retain(|(_, group)| !group.is_empty());
    }
    ordered
}
