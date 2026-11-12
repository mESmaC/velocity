//! Reading one file into its definitions, references and imports (F19). Deterministic, with no
//! model: tree-sitter for code, line rules for Markdown.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::imports::{Import, rust_use_tree};
use crate::language::Language;
use crate::text::{digest, doc_summary, qualifier, signature, type_name};

/// What one file holds. Cached by the file's content hash, so it never names the file's path.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileParse {
    pub definitions: Vec<Definition>,
    pub references: Vec<Reference>,
    pub imports: Vec<Import>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Interface,
    Type,
    Module,
    Macro,
    Constant,
    Heading,
}

impl SymbolKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Method => "method",
            Self::Class => "type",
            Self::Interface => "trait",
            Self::Type => "alias",
            Self::Module => "mod",
            Self::Macro => "macro",
            Self::Constant => "const",
            Self::Heading => "section",
        }
    }

    /// Kinds a call can land on.
    pub fn callable(self) -> bool {
        matches!(
            self,
            Self::Function | Self::Method | Self::Macro | Self::Class
        )
    }

    /// Kinds a type position or `implements` can land on.
    pub fn type_like(self) -> bool {
        matches!(self, Self::Class | Self::Interface | Self::Type)
    }
}

/// One symbol a file defines. Lines are 1-based and inclusive; bytes are offsets in the file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Definition {
    pub name: String,
    pub kind: SymbolKind,
    /// The type a method belongs to (a Rust impl's type, a class), or the trait it declares.
    pub container: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    pub name_byte: u32,
    pub signature: String,
    pub doc: String,
    /// Hash of the definition's source bytes (the span check, F19 accuracy rules).
    pub span_hash: String,
    /// A Markdown heading's level; 0 for code.
    pub level: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    /// A call to a name, or a path-qualified call (`Type::new`, `module::run`).
    Call,
    /// A method call on a receiver whose type the parser can't know (`value.run()`).
    Method,
    /// A macro invocation (`format!`): it can only land on a macro.
    Macro,
    Type,
    Implementation,
    /// A Markdown section naming a symbol in backticks.
    Mention,
    /// A Markdown section naming a file path in backticks.
    FileMention,
}

/// One use of a name.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    pub kind: ReferenceKind,
    pub qualifier: Option<String>,
    /// For a Rust `impl Trait for Type`: the type that implements `name`.
    pub from_type: Option<String>,
    pub line: u32,
    pub byte: u32,
}

/// Reads `source` as `language`.
pub fn extract(language: Language, source: &str) -> FileParse {
    match language {
        Language::Markdown => crate::markdown::extract(source),
        Language::Svelte => crate::svelte::scripts(source)
            .into_iter()
            .map(|(offset, line, script)| {
                let mut parse = extract_code(Language::Svelte, script);
                shift(&mut parse, offset, line);
                parse
            })
            .fold(FileParse::default(), merge),
        _ => extract_code(language, source),
    }
}

fn merge(mut into: FileParse, from: FileParse) -> FileParse {
    into.definitions.extend(from.definitions);
    into.references.extend(from.references);
    into.imports.extend(from.imports);
    into
}

fn shift(parse: &mut FileParse, offset: u32, lines: u32) {
    for definition in &mut parse.definitions {
        definition.start_byte += offset;
        definition.end_byte += offset;
        definition.name_byte += offset;
        definition.start_line += lines;
        definition.end_line += lines;
    }
    for reference in &mut parse.references {
        reference.byte += offset;
        reference.line += lines;
    }
    for import in &mut parse.imports {
        import.line += lines;
    }
}

/// The compiled query per language. A query that fails to compile leaves that language with
/// no symbols rather than failing every file.
fn query(language: Language) -> Option<&'static Query> {
    static QUERIES: OnceLock<std::sync::Mutex<HashMap<Language, Option<&'static Query>>>> =
        OnceLock::new();
    let cache = QUERIES.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    *cache.entry(language).or_insert_with(|| {
        let grammar = language.grammar()?;
        // Leaked once per language for the life of the process, so matches can borrow it.
        Query::new(&grammar, &language.query_source())
            .ok()
            .map(|query| &*Box::leak(Box::new(query)))
    })
}

fn extract_code(language: Language, source: &str) -> FileParse {
    let (Some(grammar), Some(query)) = (language.grammar(), query(language)) else {
        return FileParse::default();
    };
    let mut parser = Parser::new();
    if parser.set_language(&grammar).is_err() {
        return FileParse::default();
    }
    let Some(tree) = parser.parse(source, None) else {
        return FileParse::default();
    };
    let mut collector = Collector::new(language, source, query);
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    while let Some(found) = matches.next() {
        collector.take(found);
    }
    collector.finish()
}

/// A container a method sits in: a Rust impl block, with its type and trait.
struct Container {
    start: u32,
    end: u32,
    type_name: String,
    trait_name: Option<String>,
    line: u32,
}

struct Collector<'source> {
    language: Language,
    source: &'source str,
    lines: Vec<&'source str>,
    query: &'static Query,
    definitions: HashMap<u32, Definition>,
    references: HashMap<u32, Reference>,
    imports: Vec<Import>,
    containers: Vec<Container>,
}

impl<'source> Collector<'source> {
    fn new(language: Language, source: &'source str, query: &'static Query) -> Self {
        Self {
            language,
            source,
            lines: source.lines().collect(),
            query,
            definitions: HashMap::new(),
            references: HashMap::new(),
            imports: Vec::new(),
            containers: Vec::new(),
        }
    }

    fn text(&self, node: Node) -> &'source str {
        node.utf8_text(self.source.as_bytes()).unwrap_or_default()
    }

    fn take(&mut self, found: &tree_sitter::QueryMatch) {
        let mut role: Option<(&str, Node)> = None;
        let mut captures: HashMap<&str, Node> = HashMap::new();
        for capture in found.captures {
            let name = self.query.capture_names()[capture.index as usize];
            if name.starts_with("definition.")
                || name.starts_with("reference.")
                || name == "container"
                || name == "import"
            {
                role = Some((name, capture.node));
            } else {
                captures.insert(name, capture.node);
            }
        }
        match role {
            Some((role, node)) if role.starts_with("definition.") => {
                self.definition(&role["definition.".len()..], node, &captures);
            }
            Some((role, _)) if role.starts_with("reference.") => {
                self.reference(&role["reference.".len()..], &captures);
            }
            Some(("container", node)) => self.container(node, &captures),
            Some(("import", node)) => self.import(node, &captures),
            _ => {}
        }
    }

    fn definition(&mut self, role: &str, node: Node, captures: &HashMap<&str, Node>) {
        let Some(name_node) = captures.get("name") else {
            return;
        };
        let name = self.text(*name_node).to_owned();
        if name.is_empty() {
            return;
        }
        let kind = match role {
            "function" => SymbolKind::Function,
            "method" => SymbolKind::Method,
            "interface" => SymbolKind::Interface,
            "type" => SymbolKind::Type,
            "module" | "namespace" => SymbolKind::Module,
            "macro" => SymbolKind::Macro,
            "constant" => SymbolKind::Constant,
            _ => SymbolKind::Class,
        };
        let name_byte = name_node.start_byte() as u32;
        if let Some(existing) = self.definitions.get(&name_byte)
            && priority(existing.kind) >= priority(kind)
        {
            return;
        }
        let (start, end) = (node.start_byte(), node.end_byte());
        let start_line = node.start_position().row as u32 + 1;
        let definition = Definition {
            name,
            kind,
            container: None,
            start_line,
            end_line: node.end_position().row as u32 + 1,
            start_byte: start as u32,
            end_byte: end as u32,
            name_byte,
            signature: signature(self.source, start, end),
            doc: doc_summary(&self.lines, start_line as usize),
            span_hash: digest(self.source.as_bytes().get(start..end).unwrap_or_default()),
            level: 0,
        };
        self.definitions.insert(name_byte, definition);
    }

    fn reference(&mut self, role: &str, captures: &HashMap<&str, Node>) {
        let Some(name_node) = captures.get("name") else {
            return;
        };
        let name = self.text(*name_node);
        if name.is_empty() {
            return;
        }
        let kind = match role {
            "call" | "class" => ReferenceKind::Call,
            "method" => ReferenceKind::Method,
            "macro" => ReferenceKind::Macro,
            "implementation" => ReferenceKind::Implementation,
            _ => ReferenceKind::Type,
        };
        let byte = name_node.start_byte() as u32;
        let qualifier = captures
            .get("qualifier")
            .map(|node| qualifier(self.text(*node)))
            .filter(|text| !text.is_empty());
        // Two patterns can match one call (with and without its receiver); keep the one that
        // knows more.
        if let Some(existing) = self.references.get(&byte)
            && (existing.qualifier.is_some() || qualifier.is_none())
        {
            return;
        }
        // The name's line, not the call's: a call that starts a chain over several lines is
        // reported where the method is named.
        let line = name_node.start_position().row as u32 + 1;
        self.references.insert(
            byte,
            Reference {
                name: name.to_owned(),
                kind,
                qualifier,
                from_type: None,
                line,
                byte,
            },
        );
    }

    fn container(&mut self, node: Node, captures: &HashMap<&str, Node>) {
        let Some(type_node) = captures.get("container.name") else {
            return;
        };
        self.containers.push(Container {
            start: node.start_byte() as u32,
            end: node.end_byte() as u32,
            type_name: type_name(self.text(*type_node)),
            trait_name: captures
                .get("container.trait")
                .map(|node| type_name(self.text(*node))),
            line: node.start_position().row as u32 + 1,
        });
    }

    fn import(&mut self, node: Node, captures: &HashMap<&str, Node>) {
        let line = node.start_position().row as u32 + 1;
        if let Some(path) = captures.get("import.path") {
            self.imports.extend(rust_use_tree(self.text(*path), line));
            return;
        }
        let Some(name) = captures.get("import.name") else {
            return;
        };
        let name = self.text(*name);
        let source = captures
            .get("import.source")
            .map_or(name, |node| self.text(*node));
        let local = if captures.contains_key("import.source") {
            name.rsplit('.').next().unwrap_or(name)
        } else {
            // `import a.b` binds `a`.
            name.split('.').next().unwrap_or(name)
        };
        self.imports.push(Import {
            name: local.to_owned(),
            source: source.to_owned(),
            line,
        });
    }

    fn finish(mut self) -> FileParse {
        let names: std::collections::HashSet<u32> = self.definitions.keys().copied().collect();
        self.assign_containers();
        let mut definitions: Vec<Definition> = self.definitions.into_values().collect();
        definitions.sort_by_key(|definition| definition.start_byte);
        let mut references: Vec<Reference> = self
            .references
            .into_values()
            .filter(|reference| !names.contains(&reference.byte))
            .collect();
        references.extend(self.containers.iter().filter_map(implements));
        references.sort_by_key(|reference| reference.byte);
        let precise = self.language.precise();
        FileParse {
            definitions,
            references: references
                .into_iter()
                .map(|mut reference| {
                    if !precise && reference.kind == ReferenceKind::Call {
                        // Tags queries don't separate method calls, so their calls are never
                        // treated as known-receiver calls.
                        reference.kind = ReferenceKind::Method;
                    }
                    reference
                })
                .collect(),
            imports: self.imports,
        }
    }

    /// Functions inside an impl, a trait or a class become methods of that type.
    fn assign_containers(&mut self) {
        let types: Vec<(u32, u32, String)> = self
            .definitions
            .values()
            .filter(|definition| {
                matches!(definition.kind, SymbolKind::Class | SymbolKind::Interface)
            })
            .map(|definition| {
                (
                    definition.start_byte,
                    definition.end_byte,
                    definition.name.clone(),
                )
            })
            .chain(
                self.containers
                    .iter()
                    .map(|container| (container.start, container.end, container.type_name.clone())),
            )
            .collect();
        for definition in self.definitions.values_mut() {
            if !matches!(definition.kind, SymbolKind::Function | SymbolKind::Method) {
                continue;
            }
            let innermost = types
                .iter()
                .filter(|(start, end, _)| {
                    *start < definition.start_byte && definition.end_byte <= *end
                })
                .min_by_key(|(start, end, _)| end - start);
            if let Some((_, _, name)) = innermost {
                definition.kind = SymbolKind::Method;
                definition.container = Some(name.clone());
            }
        }
    }
}

fn implements(container: &Container) -> Option<Reference> {
    let trait_name = container.trait_name.clone()?;
    Some(Reference {
        name: trait_name,
        kind: ReferenceKind::Implementation,
        qualifier: None,
        from_type: Some(container.type_name.clone()),
        line: container.line,
        byte: container.start,
    })
}

/// When two patterns define the same name node, the more specific kind wins.
fn priority(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Method => 4,
        SymbolKind::Function | SymbolKind::Macro => 3,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Type | SymbolKind::Module => 2,
        SymbolKind::Constant => 1,
        SymbolKind::Heading => 0,
    }
}
