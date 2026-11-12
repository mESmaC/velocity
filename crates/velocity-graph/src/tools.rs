//! The four graph tools over one worktree's index, with the staleness check: before an answer
//! leaves, the files it names are compared with the disk, and a changed file is re-indexed and
//! the answer made again (F19 "Never stale").

use std::fmt::Write;

use serde_json::Value;

use crate::Error;
use crate::graph::RecordKind;
use crate::index::Index;
use crate::program::run;
use crate::query::{Lookup, failure, impact, line, lookup, read, within_budget};
use crate::search::Search;

/// Lines of source one record may show.
const SOURCE_LINES: usize = 150;

pub struct Tools {
    index: Index,
    search: Search,
}

/// An answer before the staleness check: its text, the files it relies on, and the records
/// whose source lines to add once the check passes.
struct Draft {
    text: String,
    files: Vec<String>,
    source: Vec<usize>,
}

impl Tools {
    pub fn new(index: Index) -> Self {
        let search = Search::new(&index.graph, None);
        Self { index, search }
    }

    /// The session-start map (F19): the worktree, its size, its biggest subsystems and its
    /// most-used symbols, in about 250 tokens, so an agent starts oriented.
    pub fn map(&self) -> String {
        let graph = &self.index.graph;
        let records = &graph.records;
        let code = |record: &crate::graph::Record| matches!(record.kind, RecordKind::Symbol(kind) if kind != crate::SymbolKind::Heading);
        let symbols = records.iter().filter(|record| code(record)).count();
        let mut subsystems: Vec<usize> = (0..records.len())
            .filter(|index| records[*index].kind == RecordKind::Subsystem)
            .collect();
        subsystems.sort_by_key(|index| std::cmp::Reverse(records[*index].down.len()));
        let shown: Vec<String> = subsystems
            .iter()
            .take(12)
            .map(|index| {
                format!(
                    "{} ({}/, {})",
                    records[*index].name,
                    records[*index].path,
                    records[*index].down.len()
                )
            })
            .collect();
        let more = subsystems.len().saturating_sub(12);
        let mut used: Vec<usize> = (0..records.len())
            .filter(|index| {
                code(&records[*index]) && !crate::search::is_test_path(&records[*index].path)
            })
            .filter(|index| !crate::resolve::is_standard_name(&records[*index].name))
            .collect();
        used.sort_by_key(|index| std::cmp::Reverse(graph.incoming[*index].len()));
        let hubs: Vec<String> = used
            .iter()
            .take(8)
            .map(|index| {
                format!(
                    "{} {} {} ({})",
                    records[*index].address,
                    records[*index].kind.label(),
                    records[*index].qualified,
                    records[*index].path
                )
            })
            .collect();
        let mut map = format!(
            "Map of {}: {} files, {symbols} symbols.
Subsystems (files): {}",
            self.index.root().display(),
            self.index.report.files,
            shown.join(", ")
        );
        if more > 0 {
            map.push_str(&format!(", and {more} more"));
        }
        map.push_str(&format!(
            ".
Most used: {}.",
            hubs.join("; ")
        ));
        map
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Re-reads `paths` (known files that changed) and lists the worktree again when a path
    /// is new to the index.
    pub fn refresh(&mut self, paths: &[String]) -> Result<(), Error> {
        let (known, unknown): (Vec<String>, Vec<String>) = paths
            .iter()
            .cloned()
            .partition(|path| self.index.contains(path));
        let mut changed = self.index.refresh(&known)?;
        // A folder's own event says nothing a file event won't; only a file new to the index
        // (or one that vanished) needs the worktree listed again.
        let root = self.index.root();
        if unknown.iter().any(|path| !root.join(path).is_dir()) {
            changed |= self.index.refresh_listing()?;
        }
        if changed {
            let previous = std::mem::replace(&mut self.search, Search::empty());
            self.search = Search::new(&self.index.graph, Some(previous));
        }
        Ok(())
    }

    /// Runs tool `name`. An `Err` is a failure the agent should read (ambiguous, missing,
    /// stale, a bad argument), not a fault.
    pub fn call(&mut self, name: &str, arguments: &Value) -> Result<String, String> {
        let mut draft = self.draft(name, arguments)?;
        let files: Vec<&str> = draft.files.iter().map(String::as_str).collect();
        let stale = self.index.stale(&files);
        if !stale.is_empty() {
            self.refresh(&stale).map_err(|error| error.to_string())?;
            draft = self.draft(name, arguments)?;
        }
        let mut text = draft.text;
        for record in draft.source {
            text.push_str(&self.source(record));
        }
        Ok(within_budget(text))
    }

    fn draft(&self, name: &str, arguments: &Value) -> Result<Draft, String> {
        match name {
            "graph_find" => Ok(self.find(arguments)),
            "graph_read" => self.read(arguments),
            "graph_impact" => self.impact(arguments),
            "graph_run" => {
                let program = text(arguments, "program")?;
                let outcome = run(&self.index.graph, &self.search, program)?;
                Ok(Draft {
                    text: outcome.text,
                    files: outcome.files,
                    source: outcome.source,
                })
            }
            other => Err(format!("unknown graph tool `{other}`")),
        }
    }

    fn find(&self, arguments: &Value) -> Draft {
        let graph = &self.index.graph;
        let query = arguments["query"].as_str().unwrap_or_default();
        let limit = arguments["limit"].as_u64().unwrap_or(10).clamp(1, 30) as usize;
        let plane = arguments["plane"].as_u64();
        let kind = arguments["kind"].as_str().map(str::to_ascii_lowercase);
        let hits = self.search.find(graph, query, limit, |record| {
            let record = &graph.records[record];
            plane.is_none_or(|plane| u64::from(record.kind.plane()) == plane)
                && kind
                    .as_deref()
                    .is_none_or(|kind| record.kind.label() == kind)
        });
        let mut text = String::new();
        let mut files = Vec::new();
        if hits.is_empty() {
            let _ = writeln!(text, "nothing matches `{query}`");
        }
        for hit in hits {
            let marker = if hit.exact { "  [exact]" } else { "" };
            let _ = writeln!(text, "{}{marker}", line(graph, hit.record));
            files.push(graph.records[hit.record].path.clone());
        }
        Draft {
            text,
            files,
            source: Vec::new(),
        }
    }

    fn read(&self, arguments: &Value) -> Result<Draft, String> {
        let graph = &self.index.graph;
        let target = text(arguments, "target")?;
        let record = match lookup(graph, &self.search, target) {
            Lookup::Found(record) => record,
            other => return Err(failure(graph, target, &other)),
        };
        let with_source = arguments["source"].as_bool().unwrap_or(false);
        Ok(Draft {
            text: read(graph, record),
            files: vec![graph.records[record].path.clone()],
            source: if with_source {
                vec![record]
            } else {
                Vec::new()
            },
        })
    }

    fn impact(&self, arguments: &Value) -> Result<Draft, String> {
        let graph = &self.index.graph;
        let targets: Vec<String> = match &arguments["targets"] {
            Value::String(one) => vec![one.clone()],
            Value::Array(many) => many
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => return Err("targets is an address, name or path, or a list of them".into()),
        };
        let mut records = Vec::new();
        for target in &targets {
            match lookup(graph, &self.search, target) {
                Lookup::Found(record) => records.push(record),
                other => return Err(failure(graph, target, &other)),
            }
        }
        let depth = arguments["depth"].as_u64().unwrap_or(2).clamp(1, 6) as usize;
        let files = records
            .iter()
            .map(|record| graph.records[*record].path.clone())
            .collect();
        Ok(Draft {
            text: impact(graph, &records, depth),
            files,
            source: Vec::new(),
        })
    }

    /// A record's lines, numbered, read after the staleness check so they match the record.
    fn source(&self, record: usize) -> String {
        let record = &self.index.graph.records[record];
        if record.kind == RecordKind::Subsystem {
            return String::new();
        }
        let Ok(content) = std::fs::read_to_string(self.index.root().join(&record.path)) else {
            return format!("(could not read {})\n", record.path);
        };
        let (start, end) = match record.kind {
            RecordKind::File => (1, SOURCE_LINES),
            _ => (record.start_line as usize, record.end_line as usize),
        };
        let total = (end + 1).saturating_sub(start);
        let mut out = format!("--- {}:{start}-{end}\n", record.path);
        let lines = content.lines().enumerate().skip(start.saturating_sub(1));
        for (number, text) in lines.take(total.min(SOURCE_LINES)) {
            let _ = writeln!(out, "{:>5}| {text}", number + 1);
        }
        if total > SOURCE_LINES {
            let _ = writeln!(
                out,
                "… {} more lines; read the file from line {}",
                total - SOURCE_LINES,
                start + SOURCE_LINES
            );
        }
        out
    }
}

fn text<'value>(arguments: &'value Value, key: &str) -> Result<&'value str, String> {
    arguments[key]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("`{key}` is required"))
}
