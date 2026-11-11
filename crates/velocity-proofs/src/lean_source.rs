//! Reading Lean source without running Lean (SPEC.md F17 "What Velocity checks"): the
//! `sorry` and `admit` scan, theorem statements for the statement lock, and whether a design
//! proof names its SPEC.md section.
//!
//! Lean comments and string literals are blanked out first, so a `sorry` in a docstring or a
//! message never counts and a statement's hash ignores comment edits.

/// One Lean file, with its path relative to the repository (forward slashes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofFile {
    pub path: String,
    pub text: String,
}

impl ProofFile {
    pub fn new(path: &str, text: &str) -> Self {
        Self {
            path: path.replace('\\', "/"),
            text: text.to_owned(),
        }
    }
}

/// A `sorry` or `admit` left in a proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placeholder {
    pub path: String,
    /// 1-based.
    pub line: usize,
    pub word: String,
}

/// A theorem's statement: everything between its name and `:=`, comments removed and
/// whitespace collapsed, keyed by file and namespace-qualified name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TheoremStatement {
    pub key: String,
    pub statement: String,
}

const PLACEHOLDERS: [&str; 2] = ["sorry", "admit"];

/// Every `sorry` and `admit` in the file's code.
pub fn placeholders(file: &ProofFile) -> Vec<Placeholder> {
    let code = code_only(&file.text);
    words(&code)
        .filter(|word| PLACEHOLDERS.contains(&word.text))
        .map(|word| Placeholder {
            path: file.path.clone(),
            line: code[..word.start].matches('\n').count() + 1,
            word: word.text.to_owned(),
        })
        .collect()
}

/// Whether the file names the SPEC.md section it covers: a line that mentions `SPEC.md`
/// together with a `§` section, `section 2.2`, or a feature number such as `F14`
/// (F17 Design proof).
pub fn names_spec_section(text: &str) -> bool {
    text.lines()
        .filter(|line| line.contains("SPEC.md"))
        .any(|line| {
            line.contains('§')
                || names_numbered_section(line)
                || words(line).any(|word| {
                    let mut characters = word.text.chars();
                    characters.next() == Some('F')
                        && !characters.as_str().is_empty()
                        && characters.as_str().chars().all(|c| c.is_ascii_digit())
                })
        })
}

fn names_numbered_section(line: &str) -> bool {
    let Some((_, reference)) = line.split_once("SPEC.md") else {
        return false;
    };
    let mut tokens = reference.split_whitespace();
    while let Some(token) = tokens.next() {
        if token != "section" {
            continue;
        }
        let Some(number) = tokens.next() else {
            return false;
        };
        // Prose punctuation can follow the section; each dotted component still
        // needs digits so an unspecified or malformed section cannot pass.
        let number = number.trim_end_matches([':', ',', ';', ')']);
        let number = number.strip_suffix('.').unwrap_or(number);
        if number.split('.').all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return true;
        }
    }
    false
}

/// Every `theorem` in the file, with its statement.
pub fn theorem_statements(file: &ProofFile) -> Vec<TheoremStatement> {
    let code = code_only(&file.text);
    let all: Vec<Word> = words(&code).collect();
    let mut scopes: Vec<Scope> = Vec::new();
    let mut statements = Vec::new();
    for (index, word) in all.iter().enumerate() {
        let next = all
            .get(index + 1)
            .filter(|next| same_line(&code, word.start, next.start));
        match word.text {
            "namespace" => {
                if let Some(name) = next {
                    scopes.push(Scope::Namespace(name.text.to_owned()));
                }
            }
            "section" => scopes.push(Scope::Section(next.map(|name| name.text.to_owned()))),
            "end" => close_scope(&mut scopes, next.map(|name| name.text)),
            "theorem" => {
                let Some(name) = all.get(index + 1) else {
                    continue;
                };
                let statement = statement_after(&code, name.start + name.text.len());
                statements.push(TheoremStatement {
                    key: format!("{}::{}", file.path, qualified(&scopes, name.text)),
                    statement,
                });
            }
            _ => {}
        }
    }
    statements
}

enum Scope {
    Namespace(String),
    Section(Option<String>),
}

/// `end` closes the innermost scope; `end A.B` closes namespace `A.B` (or section `A.B`).
fn close_scope(scopes: &mut Vec<Scope>, name: Option<&str>) {
    let Some(name) = name else {
        scopes.pop();
        return;
    };
    let matches = |scope: &Scope| match scope {
        Scope::Namespace(namespace) => namespace == name,
        Scope::Section(section) => section.as_deref() == Some(name),
    };
    if let Some(position) = scopes.iter().rposition(matches) {
        scopes.truncate(position);
    }
}

fn qualified(scopes: &[Scope], name: &str) -> String {
    if let Some(root) = name.strip_prefix("_root_.") {
        return root.to_owned();
    }
    let mut parts: Vec<&str> = scopes
        .iter()
        .filter_map(|scope| match scope {
            Scope::Namespace(namespace) => Some(namespace.as_str()),
            Scope::Section(_) => None,
        })
        .collect();
    parts.push(name);
    parts.join(".")
}

/// The statement after a theorem's name: up to `:=` at bracket depth 0, a `where`, or a line
/// that starts a pattern-matching alternative (`|`).
fn statement_after(code: &str, start: usize) -> String {
    let rest = &code[start..];
    let mut depth = 0usize;
    let mut end = rest.len();
    let mut at_line_start = false;
    for (offset, character) in rest.char_indices() {
        match character {
            '(' | '[' | '{' | '⟨' | '⦃' => depth += 1,
            ')' | ']' | '}' | '⟩' | '⦄' => depth = depth.saturating_sub(1),
            ':' if depth == 0 && rest[offset..].starts_with(":=") => {
                end = offset;
                break;
            }
            '|' if depth == 0 && at_line_start => {
                end = offset;
                break;
            }
            'w' if depth == 0 && starts_word(rest, offset, "where") => {
                end = offset;
                break;
            }
            _ => {}
        }
        if character == '\n' {
            at_line_start = true;
        } else if !character.is_whitespace() {
            at_line_start = false;
        }
    }
    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

fn starts_word(text: &str, offset: usize, word: &str) -> bool {
    text[offset..].starts_with(word)
        && !text[..offset]
            .chars()
            .next_back()
            .is_some_and(is_word_character)
        && !text[offset + word.len()..]
            .chars()
            .next()
            .is_some_and(is_word_character)
}

fn same_line(code: &str, from: usize, to: usize) -> bool {
    !code[from..to].contains('\n')
}

struct Word<'a> {
    start: usize,
    text: &'a str,
}

/// Identifier-like words: letters, digits, `_`, `'`, `!`, `?` and `.` (so `Foo.sorry` is one
/// word and never counts as `sorry`).
fn words(code: &str) -> impl Iterator<Item = Word<'_>> {
    let mut position = 0;
    std::iter::from_fn(move || {
        let rest = &code[position..];
        let start = position + rest.find(is_word_character)?;
        let length = code[start..]
            .find(|character| !is_word_character(character))
            .unwrap_or(code.len() - start);
        position = start + length;
        Some(Word {
            start,
            text: &code[start..start + length],
        })
    })
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '\'' | '!' | '?' | '.')
}

/// The source with comments, string literals and character literals replaced by spaces.
/// Newlines are kept, so line numbers still match the original.
pub(crate) fn code_only(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < characters.len() {
        let skipped = match (characters[index], characters.get(index + 1)) {
            ('-', Some('-')) => line_comment(&characters, index),
            ('/', Some('-')) => block_comment(&characters, index),
            ('"', _) => string_literal(&characters, index),
            ('\'', _) if !previous_is_word(&characters, index) => {
                character_literal(&characters, index)
            }
            _ => None,
        };
        match skipped {
            Some(end) => {
                for character in &characters[index..end] {
                    output.push(blank(*character));
                }
                index = end;
            }
            None => {
                output.push(characters[index]);
                index += 1;
            }
        }
    }
    output
}

/// Offsets only need to agree within the blanked text, and line numbers only count newlines.
fn blank(character: char) -> char {
    if character == '\n' { '\n' } else { ' ' }
}

fn previous_is_word(characters: &[char], index: usize) -> bool {
    index > 0 && is_word_character(characters[index - 1])
}

fn line_comment(characters: &[char], start: usize) -> Option<usize> {
    let end = characters[start..]
        .iter()
        .position(|character| *character == '\n')
        .map_or(characters.len(), |offset| start + offset);
    Some(end)
}

/// `/- ... -/`, nested as Lean nests them.
fn block_comment(characters: &[char], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = start;
    while index + 1 < characters.len() {
        match (characters[index], characters[index + 1]) {
            ('/', '-') => {
                depth += 1;
                index += 2;
            }
            ('-', '/') => {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => index += 1,
        }
    }
    Some(characters.len())
}

fn string_literal(characters: &[char], start: usize) -> Option<usize> {
    let mut index = start + 1;
    while index < characters.len() {
        match characters[index] {
            '\\' => index += 2,
            '"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    Some(characters.len())
}

/// `'a'` or `'\n'`; anything else starting with `'` is left alone.
fn character_literal(characters: &[char], start: usize) -> Option<usize> {
    let close = match characters.get(start + 1) {
        Some('\\') => start + 3,
        Some(_) => start + 2,
        None => return None,
    };
    (characters.get(close) == Some(&'\'')).then_some(close + 1)
}
