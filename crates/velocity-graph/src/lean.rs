//! Lean declarations and module imports. Elaboration and tactic calls are outside this index.

use crate::extract::{Definition, FileParse, SymbolKind};
use crate::imports::Import;

pub fn extract(source: &str) -> FileParse {
    let mut parse = FileParse::default();
    let mut scopes = Vec::new();
    let mut pending = None;
    let mut offset = 0;
    let mut comment_depth = 0;
    for (line, raw) in source.split_inclusive('\n').enumerate() {
        let code = without_comments(raw, &mut comment_depth);
        let mut words = code.split_whitespace().skip_while(|word| {
            matches!(
                *word,
                "private"
                    | "protected"
                    | "public"
                    | "local"
                    | "scoped"
                    | "noncomputable"
                    | "unsafe"
                    | "partial"
            )
        });
        let command = words.next().unwrap_or_default();
        let argument = words.next().unwrap_or_default();
        let kind = declaration(command);
        if kind.is_some()
            || matches!(
                command,
                "end" | "namespace" | "section" | "open" | "import" | "example"
            )
            || command.starts_with("#")
        {
            finish(source, &mut parse, pending.take(), offset);
        }
        match command {
            "namespace" => scopes.push(Some(argument.to_owned())),
            "section" => scopes.push(None),
            "end" => {
                scopes.pop();
            }
            "import" => {
                for module in std::iter::once(argument)
                    .chain(words)
                    .filter(|word| !word.is_empty())
                {
                    parse.imports.push(Import {
                        name: "*".into(),
                        source: module.to_owned(),
                        original: None,
                        line: line as u32 + 1,
                        reexport: false,
                    });
                }
            }
            _ => {
                let name = argument.trim_end_matches([':', '(']);
                if let Some(kind) = kind
                    && name.starts_with(|character: char| {
                        character.is_alphabetic() || character == '_'
                    })
                {
                    let namespace = scopes
                        .iter()
                        .filter_map(Option::as_deref)
                        .collect::<Vec<_>>()
                        .join(".");
                    pending = Some((offset, kind, name.to_owned(), namespace));
                }
            }
        }
        offset += raw.len();
    }
    finish(source, &mut parse, pending, source.len());
    parse
}

type Declaration = (usize, SymbolKind, String, String);

fn finish(source: &str, parse: &mut FileParse, declaration: Option<Declaration>, end: usize) {
    if let Some((start, kind, name, namespace)) = declaration {
        let mut definition = Definition::at(source, start, end, name, kind);
        definition.container = (!namespace.is_empty()).then_some(namespace);
        parse.definitions.push(definition);
    }
}

fn declaration(word: &str) -> Option<SymbolKind> {
    Some(match word {
        "theorem" | "lemma" => SymbolKind::Theorem,
        "def" | "abbrev" | "opaque" => SymbolKind::Function,
        "axiom" | "constant" | "instance" => SymbolKind::Constant,
        "structure" | "inductive" | "class" => SymbolKind::Class,
        _ => return None,
    })
}

fn without_comments(line: &str, depth: &mut usize) -> String {
    let mut output = String::new();
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        match (character, chars.peek()) {
            ('/', Some('-')) => {
                *depth += 1;
                chars.next();
            }
            ('-', Some('/')) if *depth > 0 => {
                *depth -= 1;
                chars.next();
                output.push(' ');
            }
            ('-', Some('-')) if *depth == 0 => break,
            ('"', _) if *depth == 0 => {
                output.push(' ');
                while let Some(next) = chars.next() {
                    match next {
                        '"' => break,
                        '\\' => {
                            chars.next();
                        }
                        _ => {}
                    }
                }
            }
            _ if *depth == 0 => output.push(character),
            _ => {}
        }
    }
    output
}
