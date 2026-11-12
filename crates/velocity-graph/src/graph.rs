//! The graph (F19 "What it holds"): records on three planes, their short addresses, and edges
//! that say how they were found.

use std::collections::HashMap;
use std::sync::Arc;

use crate::extract::{FileParse, SymbolKind};
use crate::language::Language;
use crate::subsystems::Subsystem;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecordKind {
    Symbol(SymbolKind),
    File,
    Subsystem,
}

impl RecordKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Symbol(kind) => kind.label(),
            Self::File => "file",
            Self::Subsystem => "subsystem",
        }
    }

    pub fn plane(self) -> u8 {
        match self {
            Self::Symbol(_) => 1,
            Self::File => 2,
            Self::Subsystem => 3,
        }
    }
}

/// One record. Symbols carry their file's lines and span hash; files and subsystems carry
/// their path.
#[derive(Clone, Debug)]
pub struct Record {
    pub address: String,
    pub kind: RecordKind,
    pub name: String,
    /// `Type::method` for a method, the name otherwise.
    pub qualified: String,
    /// The file (symbols and files) or folder (subsystems).
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    pub signature: String,
    pub doc: String,
    pub span_hash: String,
    pub up: Option<usize>,
    pub down: Vec<usize>,
    pub subsystem: usize,
    /// The method's type, for `self.` calls.
    pub container: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Relation {
    Calls,
    Implements,
    References,
    Mentions,
    Imports,
    Depends,
}

impl Relation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Implements => "implements",
            Self::References => "references",
            Self::Mentions => "mentions",
            Self::Imports => "imports",
            Self::Depends => "depends",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.to_ascii_lowercase().trim_end_matches('s') {
            "call" => Self::Calls,
            "implement" => Self::Implements,
            "reference" | "use" => Self::References,
            "mention" => Self::Mentions,
            "import" => Self::Imports,
            "depend" => Self::Depends,
            _ => return None,
        })
    }
}

/// How an edge was found (F19 accuracy rules).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// Explicit in the source.
    Extracted,
    /// Resolved by name across files; the only candidate.
    Inferred,
    /// The name matched this many definitions and nothing narrows it down; this is one of
    /// them. With a count of 1, the only candidate's type wasn't in play at the call.
    Ambiguous(u16),
}

impl Provenance {
    pub fn label(self) -> String {
        match self {
            Self::Extracted => "extracted".into(),
            Self::Inferred => "inferred".into(),
            // One name match whose receiver type isn't in play: a lead, not a link.
            Self::Ambiguous(1) => "unconfirmed".into(),
            Self::Ambiguous(count) => format!("ambiguous 1 of {count}"),
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Extracted => 0,
            Self::Inferred => 1,
            Self::Ambiguous(_) => 2,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub relation: Relation,
    pub provenance: Provenance,
    /// The line in `from`'s file where the edge was found.
    pub line: u32,
}

/// What a graph is built from: each file's path, hash, language and parse.
pub struct FileInput {
    pub path: String,
    pub hash: String,
    pub language: Option<Language>,
    pub parse: Arc<FileParse>,
}

pub struct Graph {
    pub records: Vec<Record>,
    pub edges: Vec<Edge>,
    pub outgoing: Vec<Vec<usize>>,
    pub incoming: Vec<Vec<usize>>,
    pub by_address: HashMap<String, usize>,
    /// Lower-cased names and qualified names to records.
    pub by_name: HashMap<String, Vec<usize>>,
    pub by_path: HashMap<String, usize>,
    pub file_hashes: HashMap<String, String>,
    pub subsystems: Vec<Subsystem>,
}

impl Graph {
    pub fn build(
        files: &[FileInput],
        subsystems: Vec<Subsystem>,
        manifests: &HashMap<String, String>,
    ) -> Self {
        let mut builder = crate::build::Builder::new(subsystems);
        for file in files {
            builder.add_file(file);
        }
        let mut edges = builder.dependencies(manifests);
        let (records, subsystems) = builder.finish();
        let mut graph = Self::index(records, subsystems, files);
        let resolved = crate::resolve::edges(&graph, files, &edges);
        edges.extend(resolved);
        graph.set_edges(edges);
        graph
    }

    fn index(records: Vec<Record>, subsystems: Vec<Subsystem>, files: &[FileInput]) -> Self {
        let mut by_address = HashMap::new();
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_path = HashMap::new();
        for (index, record) in records.iter().enumerate() {
            by_address.insert(record.address.clone(), index);
            by_name
                .entry(record.name.to_lowercase())
                .or_default()
                .push(index);
            if record.qualified != record.name {
                by_name
                    .entry(record.qualified.to_lowercase())
                    .or_default()
                    .push(index);
            }
            if record.kind == RecordKind::File {
                by_path.insert(record.path.clone(), index);
            }
        }
        Self {
            outgoing: vec![Vec::new(); records.len()],
            incoming: vec![Vec::new(); records.len()],
            records,
            edges: Vec::new(),
            by_address,
            by_name,
            by_path,
            file_hashes: files
                .iter()
                .map(|file| (file.path.clone(), file.hash.clone()))
                .collect(),
            subsystems,
        }
    }

    /// Keeps the best-found edge per (from, to, relation) and indexes both directions.
    fn set_edges(&mut self, edges: Vec<Edge>) {
        let mut best: HashMap<(usize, usize, Relation), Edge> = HashMap::new();
        for edge in edges.into_iter().filter(|edge| edge.from != edge.to) {
            best.entry((edge.from, edge.to, edge.relation))
                .and_modify(|kept| {
                    if edge.provenance.rank() < kept.provenance.rank() {
                        *kept = edge;
                    }
                })
                .or_insert(edge);
        }
        let mut edges: Vec<Edge> = best.into_values().collect();
        edges.sort_by_key(|edge| (edge.from, edge.relation.label(), edge.to));
        for (index, edge) in edges.iter().enumerate() {
            self.outgoing[edge.from].push(index);
            self.incoming[edge.to].push(index);
        }
        self.edges = edges;
    }

    /// The records named `name` (case-insensitive name or qualified name).
    pub fn named(&self, name: &str) -> &[usize] {
        self.by_name
            .get(&name.to_lowercase())
            .map_or(&[], Vec::as_slice)
    }
}
