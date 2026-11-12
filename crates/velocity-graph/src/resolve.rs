//! Name resolution (F19 accuracy rules): every reference becomes edges that say how they were
//! found, and a name the parser can't pin down stays `ambiguous` instead of being guessed.

use std::collections::{HashMap, HashSet};

use crate::extract::{Reference, ReferenceKind, SymbolKind};
use crate::graph::{Edge, FileInput, Graph, Provenance, RecordKind, Relation};
use crate::imports::Import;
use crate::language::Language;

/// A name matching more definitions than this is too common to link at all.
const AMBIGUITY_LIMIT: usize = 12;

type Targets = Vec<(usize, Provenance)>;

pub fn edges(graph: &Graph, files: &[FileInput], depends: &[Edge]) -> Vec<Edge> {
    let context = Context::new(graph, files, depends);
    let mut edges = Vec::new();
    for file in files {
        let Some(&record) = graph.by_path.get(&file.path) else {
            continue;
        };
        let scope = Scope {
            context: &context,
            file,
            record,
            subsystem: graph.records[record].subsystem,
        };
        edges.extend(scope.import_edges());
        for reference in &file.parse.references {
            edges.extend(scope.reference_edges(reference));
        }
    }
    edges
}

struct Context<'graph> {
    graph: &'graph Graph,
    /// Code symbols (not headings) by exact name.
    symbols: HashMap<&'graph str, Vec<usize>>,
    languages: HashMap<&'graph str, Option<Language>>,
    /// Subsystem records by the name Rust code uses (`velocity_git`) and the package name.
    packages: HashMap<String, usize>,
    /// For each subsystem record, the subsystems its code can see: itself and what its
    /// manifest depends on, transitively. Absent for folders with no manifest: they see all.
    visible: HashMap<usize, HashSet<usize>>,
}

impl<'graph> Context<'graph> {
    fn new(graph: &'graph Graph, files: &'graph [FileInput], depends: &[Edge]) -> Self {
        let mut symbols: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut packages = HashMap::new();
        for (index, record) in graph.records.iter().enumerate() {
            match record.kind {
                RecordKind::Symbol(SymbolKind::Heading) | RecordKind::File => {}
                RecordKind::Symbol(_) => {
                    symbols.entry(record.name.as_str()).or_default().push(index)
                }
                RecordKind::Subsystem => {
                    if let Some(subsystem) = graph
                        .subsystems
                        .iter()
                        .find(|subsystem| subsystem.folder == record.path)
                    {
                        packages.insert(subsystem.crate_name.clone(), index);
                        packages.insert(subsystem.name.clone(), index);
                    }
                }
            }
        }
        let manifests: HashSet<usize> = packages.values().copied().collect();
        Self {
            graph,
            symbols,
            languages: files
                .iter()
                .map(|file| (file.path.as_str(), file.language))
                .collect(),
            packages,
            visible: visibility(&manifests, depends),
        }
    }

    fn sees(&self, from_subsystem: usize, record: usize) -> bool {
        self.visible
            .get(&from_subsystem)
            .is_none_or(|visible| visible.contains(&self.graph.records[record].subsystem))
    }
}

fn visibility(packages: &HashSet<usize>, depends: &[Edge]) -> HashMap<usize, HashSet<usize>> {
    let mut direct: HashMap<usize, Vec<usize>> = HashMap::new();
    for edge in depends {
        direct.entry(edge.from).or_default().push(edge.to);
    }
    packages
        .iter()
        .map(|package| {
            let mut seen = HashSet::from([*package]);
            let mut stack = vec![*package];
            while let Some(next) = stack.pop() {
                for dependency in direct.get(&next).into_iter().flatten() {
                    if seen.insert(*dependency) {
                        stack.push(*dependency);
                    }
                }
            }
            (*package, seen)
        })
        .collect()
}

/// One of the given candidates: exact when it's the only one, ambiguous (each candidate kept,
/// tagged) when there are a few, and nothing when the name is too common to mean anything.
fn pick(candidates: &[usize], single: Provenance) -> Targets {
    match candidates.len() {
        0 => Vec::new(),
        1 => vec![(candidates[0], single)],
        count if count <= AMBIGUITY_LIMIT => candidates
            .iter()
            .map(|candidate| (*candidate, Provenance::Ambiguous(count as u16)))
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether a reference of this kind can land on a symbol of that kind. A path-qualified call
/// (`Type::new`) can land on a method; a bare call can't.
fn fits(reference: ReferenceKind, symbol: SymbolKind, precise: bool, qualified: bool) -> bool {
    match reference {
        ReferenceKind::Call if qualified => symbol.callable(),
        ReferenceKind::Call => symbol.callable() && symbol != SymbolKind::Method,
        ReferenceKind::Method if precise => symbol == SymbolKind::Method,
        ReferenceKind::Method => symbol.callable(),
        ReferenceKind::Macro => symbol == SymbolKind::Macro,
        ReferenceKind::Type | ReferenceKind::Implementation => symbol.type_like(),
        ReferenceKind::Mention => symbol != SymbolKind::Heading,
        ReferenceKind::FileMention => false,
    }
}

struct Scope<'scope, 'graph> {
    context: &'scope Context<'graph>,
    file: &'scope FileInput,
    record: usize,
    subsystem: usize,
}

impl Scope<'_, '_> {
    fn graph(&self) -> &Graph {
        self.context.graph
    }

    fn precise(&self) -> bool {
        self.file.language.is_some_and(Language::precise)
    }

    fn rust(&self) -> bool {
        self.file.language == Some(Language::Rust)
    }

    /// This file's symbols, in the order of its parse's definitions.
    fn own_symbols(&self) -> &[usize] {
        &self.graph().records[self.record].down
    }

    fn reference_edges(&self, reference: &Reference) -> Vec<Edge> {
        let relation = match reference.kind {
            ReferenceKind::Call | ReferenceKind::Method | ReferenceKind::Macro => Relation::Calls,
            ReferenceKind::Type => Relation::References,
            ReferenceKind::Implementation => Relation::Implements,
            ReferenceKind::Mention | ReferenceKind::FileMention => Relation::Mentions,
        };
        let from = self.source(reference);
        self.resolve(reference)
            .into_iter()
            .map(|(to, provenance)| Edge {
                from,
                to,
                relation,
                provenance,
                line: reference.line,
            })
            .collect()
    }

    /// The record a reference is made from: the innermost symbol around it, the type an
    /// `impl Trait for Type` names, or the file.
    fn source(&self, reference: &Reference) -> usize {
        let records = &self.graph().records;
        if let Some(type_name) = &reference.from_type
            && let Some(found) = self.own_symbols().iter().find(|symbol| {
                records[**symbol].name == *type_name
                    && records[**symbol].kind == RecordKind::Symbol(SymbolKind::Class)
            })
        {
            return *found;
        }
        self.own_symbols()
            .iter()
            .filter(|symbol| {
                let record = &records[**symbol];
                record.start_byte <= reference.byte && reference.byte < record.end_byte
            })
            .min_by_key(|symbol| records[**symbol].end_byte - records[**symbol].start_byte)
            .copied()
            .unwrap_or(self.record)
    }

    fn resolve(&self, reference: &Reference) -> Targets {
        if reference.kind == ReferenceKind::FileMention {
            return self.file_mention(&reference.name);
        }
        let precise = self.precise();
        let candidates: Vec<usize> = self
            .context
            .symbols
            .get(reference.name.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|candidate| match self.graph().records[*candidate].kind {
                RecordKind::Symbol(kind) => {
                    fits(reference.kind, kind, precise, reference.qualifier.is_some())
                }
                _ => false,
            })
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }
        if let Some(qualifier) = &reference.qualifier {
            if let Some(targets) = self.qualified(reference, qualifier, &candidates) {
                return targets;
            }
            if reference.kind != ReferenceKind::Method {
                // A path this project doesn't define (`std::fs::read`, `String::new`).
                return Vec::new();
            }
        }
        self.unqualified(reference, &candidates)
    }

    /// `Some` when the qualifier was understood: `self`, a type, an imported module or a
    /// package path. `None` when it's a value whose type the parser can't know.
    fn qualified(
        &self,
        reference: &Reference,
        qualifier: &str,
        candidates: &[usize],
    ) -> Option<Targets> {
        let records = &self.graph().records;
        let last = qualifier.rsplit("::").next().unwrap_or(qualifier);
        if matches!(last, "self" | "Self" | "this") {
            let container = records[self.source(reference)].container.clone();
            let matching: Vec<usize> = candidates
                .iter()
                .copied()
                .filter(|candidate| {
                    container.is_some() && records[*candidate].container == container
                })
                .filter(|candidate| {
                    records[*candidate].path == self.file.path
                        || self.context.sees(self.subsystem, *candidate)
                })
                .collect();
            return Some(pick(&matching, Provenance::Extracted));
        }
        let of_type: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|candidate| records[*candidate].container.as_deref() == Some(last))
            .filter(|candidate| self.context.sees(self.subsystem, *candidate))
            .collect();
        if !of_type.is_empty() {
            return Some(pick(&of_type, Provenance::Extracted));
        }
        if let Some(import) = self.import_named(qualifier) {
            let in_module = self.import_scope(import, candidates, false);
            return Some(pick(&in_module, Provenance::Extracted));
        }
        if qualifier.contains("::") || self.rust() {
            return Some(self.module_path(qualifier, candidates));
        }
        None
    }

    fn import_named(&self, name: &str) -> Option<&Import> {
        self.file
            .parse
            .imports
            .iter()
            .find(|import| import.name == name)
    }

    /// Candidates inside what an import names: a Rust module or item, or a JavaScript file.
    /// `item` says the import names the item itself (`use a::b::run;` then `run()`), not a
    /// module the call goes through (`use a::b;` then `b::run()`).
    fn import_scope(&self, import: &Import, candidates: &[usize], item: bool) -> Vec<usize> {
        if self.rust() {
            let path = if item {
                import.source.clone()
            } else {
                format!("{}::_", import.source)
            };
            return match self.rust_path_scope(&path) {
                Some((subsystem, modules)) => self.in_scope(candidates, subsystem, &modules),
                None => Vec::new(),
            };
        }
        let files = self.module_files(&import.source);
        candidates
            .iter()
            .copied()
            .filter(|candidate| files.contains(&self.graph().records[*candidate].path))
            .collect()
    }

    /// A Rust path qualifier (`crate::claims`, `velocity_git::lifecycle`, `claims`).
    fn module_path(&self, qualifier: &str, candidates: &[usize]) -> Targets {
        if let Some((subsystem, modules)) = self.rust_path_scope(&format!("{qualifier}::_")) {
            return pick(
                &self.in_scope(candidates, subsystem, &modules),
                Provenance::Extracted,
            );
        }
        if qualifier.contains("::") {
            // Starts outside the project: `std::fs::read`.
            return Vec::new();
        }
        // A bare module declared in this package (`lifecycle::plan` after `mod lifecycle;`).
        // It must match a module here, so `fs::read` never lands on a project `read`.
        let records = &self.graph().records;
        let matching: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|candidate| {
                records[*candidate].subsystem == self.subsystem
                    && path_has_module(&records[*candidate].path, qualifier)
            })
            .collect();
        pick(&matching, Provenance::Extracted)
    }

    /// For a Rust path `a::b::c::Name`: the package it starts in and the modules between.
    /// `None` for a path outside the project (`std::...`, another crate).
    fn rust_path_scope(&self, path: &str) -> Option<(usize, Vec<String>)> {
        let segments: Vec<&str> = path.split("::").collect();
        let (first, rest) = segments.split_first()?;
        let modules = rest[..rest.len().saturating_sub(1)]
            .iter()
            .filter(|segment| !matches!(**segment, "self" | "super"))
            .map(|segment| (*segment).to_owned())
            .collect();
        let subsystem = match *first {
            "crate" | "self" | "super" => self.subsystem,
            name => *self.context.packages.get(name)?,
        };
        Some((subsystem, modules))
    }

    /// Candidates in `subsystem`, narrowed to files under `modules` when that leaves any (a
    /// re-export from the crate root keeps the whole package).
    fn in_scope(&self, candidates: &[usize], subsystem: usize, modules: &[String]) -> Vec<usize> {
        let records = &self.graph().records;
        let in_package: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|candidate| records[*candidate].subsystem == subsystem)
            .collect();
        let in_modules: Vec<usize> = in_package
            .iter()
            .copied()
            .filter(|candidate| {
                modules
                    .iter()
                    .all(|module| path_has_module(&records[*candidate].path, module))
            })
            .collect();
        if in_modules.is_empty() {
            in_package
        } else {
            in_modules
        }
    }

    fn unqualified(&self, reference: &Reference, candidates: &[usize]) -> Targets {
        let records = &self.graph().records;
        let visible: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|candidate| self.context.sees(self.subsystem, *candidate))
            .collect();
        if reference.kind == ReferenceKind::Method {
            return self.method_call(reference, &visible);
        }
        if reference.kind == ReferenceKind::Mention {
            // Prose: at best a lead to check.
            return pick(&visible, Provenance::Inferred);
        }
        let same_file: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|candidate| records[*candidate].path == self.file.path)
            .collect();
        if same_file.len() == 1 && self.precise() {
            return vec![(same_file[0], Provenance::Extracted)];
        }
        if let Some(import) = self.import_named(&reference.name) {
            let imported = self.import_scope(import, candidates, true);
            if !imported.is_empty() {
                return pick(&imported, Provenance::Extracted);
            }
        }
        pick(&visible, Provenance::Inferred)
    }

    /// A call on a receiver whose type the parser can't know (`value.join()`). A candidate
    /// counts only when its type is in play in the calling code (its signature, its type uses
    /// or its own impl), so `names.join(",")` never lands on a project's `Channel::join`.
    /// Candidates whose type isn't in play stay `ambiguous`, which programs don't follow.
    fn method_call(&self, reference: &Reference, visible: &[usize]) -> Targets {
        let records = &self.graph().records;
        let in_play = self.types_in_play(self.source(reference));
        let confirmed: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|candidate| {
                records[*candidate]
                    .container
                    .as_ref()
                    .is_some_and(|container| in_play.contains(container))
            })
            .collect();
        if !confirmed.is_empty() {
            return pick(&confirmed, Provenance::Inferred);
        }
        if visible.len() > AMBIGUITY_LIMIT {
            return Vec::new();
        }
        let count = visible.len() as u16;
        visible
            .iter()
            .map(|candidate| (*candidate, Provenance::Ambiguous(count)))
            .collect()
    }

    /// Type names the code at `source` uses: the words of its signature and the types
    /// referenced inside it. Its own type doesn't count: a call on `self` resolves through
    /// `self`, so another receiver in a method (`cache.get()`) is some other type.
    fn types_in_play(&self, source: usize) -> HashSet<String> {
        let record = &self.graph().records[source];
        let mut names: HashSet<String> = record
            .signature
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .filter(|word| !word.is_empty())
            .map(str::to_owned)
            .collect();
        let whole_file = source == self.record;
        names.extend(
            self.file
                .parse
                .references
                .iter()
                .filter(|used| used.kind == ReferenceKind::Type)
                .filter(|used| {
                    whole_file || (record.start_byte <= used.byte && used.byte < record.end_byte)
                })
                .map(|used| used.name.clone()),
        );
        names
    }

    fn file_mention(&self, path: &str) -> Targets {
        let graph = self.graph();
        if let Some(record) = graph.by_path.get(path) {
            return vec![(*record, Provenance::Extracted)];
        }
        let suffix = format!("/{path}");
        let matching: Vec<usize> = graph
            .by_path
            .iter()
            .filter(|(candidate, _)| candidate.ends_with(&suffix))
            .map(|(_, record)| *record)
            .collect();
        pick(&matching, Provenance::Inferred)
    }

    /// `imports` edges from this file to the files its imports resolve to.
    fn import_edges(&self) -> Vec<Edge> {
        let graph = self.graph();
        let mut targets = HashSet::new();
        for import in &self.file.parse.imports {
            let files = if self.rust() {
                self.rust_import_files(import)
            } else {
                self.module_files(&import.source)
            };
            targets.extend(
                files
                    .into_iter()
                    .filter_map(|path| graph.by_path.get(&path).copied()),
            );
        }
        targets
            .into_iter()
            .map(|to| Edge {
                from: self.record,
                to,
                relation: Relation::Imports,
                provenance: Provenance::Extracted,
                line: 0,
            })
            .collect()
    }

    fn rust_import_files(&self, import: &Import) -> Vec<String> {
        let names = [import.source.rsplit("::").next().unwrap_or_default()];
        let candidates: Vec<usize> = names
            .iter()
            .flat_map(|name| {
                self.context
                    .symbols
                    .get(name)
                    .into_iter()
                    .flatten()
                    .copied()
            })
            .collect();
        let path = if import.name == "*" {
            format!("{}::_", import.source)
        } else {
            import.source.clone()
        };
        let Some((subsystem, modules)) = self.rust_path_scope(&path) else {
            return Vec::new();
        };
        self.in_scope(&candidates, subsystem, &modules)
            .into_iter()
            .map(|candidate| self.graph().records[candidate].path.clone())
            .collect()
    }

    /// The project files a JavaScript, TypeScript or Python module specifier names.
    fn module_files(&self, specifier: &str) -> Vec<String> {
        let folder = self
            .file
            .path
            .rsplit_once('/')
            .map_or("", |(folder, _)| folder);
        let base = if let Some(rest) = specifier.strip_prefix("$lib/") {
            let package = &self.graph().records[self.subsystem].path;
            join(package, &format!("src/lib/{rest}"))
        } else if specifier.starts_with('.')
            && !specifier.contains('/')
            && self.file.language == Some(Language::Python)
        {
            join(folder, &specifier.trim_start_matches('.').replace('.', "/"))
        } else if specifier.starts_with('.') {
            join(folder, specifier)
        } else if self.file.language == Some(Language::Python) {
            specifier.replace('.', "/")
        } else {
            return Vec::new();
        };
        const ENDINGS: [&str; 11] = [
            "",
            ".ts",
            ".tsx",
            ".js",
            ".mjs",
            ".svelte",
            ".py",
            "/index.ts",
            "/index.js",
            "/__init__.py",
            ".d.ts",
        ];
        ENDINGS
            .iter()
            .map(|ending| format!("{base}{ending}"))
            .filter(|path| {
                self.context.languages.contains_key(path.as_str())
                    || self.graph().by_path.contains_key(path)
            })
            .chain(
                // Python's absolute imports are relative to a source root we don't know.
                (self.file.language == Some(Language::Python))
                    .then(|| self.python_suffix(&base))
                    .into_iter()
                    .flatten(),
            )
            .collect()
    }

    fn python_suffix(&self, base: &str) -> Vec<String> {
        let ends = [format!("/{base}.py"), format!("/{base}/__init__.py")];
        self.graph()
            .by_path
            .keys()
            .filter(|path| ends.iter().any(|end| path.ends_with(end.as_str())))
            .cloned()
            .collect()
    }
}

/// `src/claims.rs`, `src/claims/mod.rs` and `src/claims/store.rs` are all in module `claims`.
fn path_has_module(path: &str, module: &str) -> bool {
    path.split('/').any(|component| {
        component == module
            || component
                .rsplit_once('.')
                .is_some_and(|(stem, _)| stem == module)
    })
}

/// `folder` joined with a relative path, with `.` and `..` resolved.
fn join(folder: &str, relative: &str) -> String {
    let mut parts: Vec<&str> = folder.split('/').filter(|part| !part.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}
