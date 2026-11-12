//! Markdown read for structure only (F19: no model in the index): headings become sections,
//! and code names in backticks become `mentions` from the section to the code.

use crate::extract::{Definition, FileParse, Reference, ReferenceKind, SymbolKind};
use crate::text::{digest, first_line_summary};

struct Heading {
    level: u8,
    title: String,
    line: u32,
    byte: usize,
}

pub fn extract(source: &str) -> FileParse {
    let mut headings = Vec::new();
    let mut references = Vec::new();
    let mut fenced = false;
    let mut byte = 0usize;
    for (index, line) in source.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        let number = index as u32 + 1;
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
        } else if !fenced {
            if let Some(heading) = heading(trimmed, number, byte) {
                headings.push(heading);
            } else {
                references.extend(mentions(line, number, byte));
            }
        }
        byte += line.len();
    }
    FileParse {
        definitions: sections(source, &headings),
        references,
        imports: Vec::new(),
    }
}

fn heading(line: &str, number: u32, byte: usize) -> Option<Heading> {
    let level = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if level == 0 || level > 6 || !line[level..].starts_with(' ') {
        return None;
    }
    let title = line[level..].trim().trim_end_matches('#').trim();
    (!title.is_empty()).then(|| Heading {
        level: level as u8,
        title: title.to_owned(),
        line: number,
        byte,
    })
}

/// Each heading's section runs to the next heading of the same or a higher level.
fn sections(source: &str, headings: &[Heading]) -> Vec<Definition> {
    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len() as u32;
    headings
        .iter()
        .enumerate()
        .map(|(index, heading)| {
            let next = headings[index + 1..]
                .iter()
                .find(|later| later.level <= heading.level);
            let end_byte = next.map_or(source.len(), |later| later.byte);
            let end_line = next.map_or(total_lines, |later| later.line.saturating_sub(1));
            let span = source
                .as_bytes()
                .get(heading.byte..end_byte)
                .unwrap_or_default();
            Definition {
                name: heading.title.clone(),
                kind: SymbolKind::Heading,
                container: None,
                start_line: heading.line,
                end_line: end_line.max(heading.line),
                start_byte: heading.byte as u32,
                end_byte: end_byte as u32,
                name_byte: heading.byte as u32,
                signature: format!("{} {}", "#".repeat(heading.level as usize), heading.title),
                doc: first_line_summary(&lines, heading.line as usize),
                span_hash: digest(span),
                level: heading.level,
            }
        })
        .collect()
}

/// Code names in backticks on one line. Only names that look like code count (snake_case,
/// a path, camelCase, or a call), so prose in backticks such as `true` adds nothing.
fn mentions(line: &str, number: u32, line_byte: usize) -> Vec<Reference> {
    let mut found = Vec::new();
    let mut rest = line;
    let mut offset = 0usize;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        let content = &after[..close];
        let byte = (line_byte + offset + open + 1) as u32;
        if let Some(reference) = mention(content, number, byte) {
            found.push(reference);
        }
        let consumed = open + 1 + close + 1;
        offset += consumed;
        rest = &rest[consumed..];
    }
    found
}

fn mention(content: &str, line: u32, byte: u32) -> Option<Reference> {
    let token = content
        .split([' ', '(', '{', '<'])
        .next()
        .unwrap_or_default()
        .trim_end_matches("()");
    let reference = |name: &str, kind, qualifier: Option<String>| Reference {
        name: name.to_owned(),
        kind,
        qualifier,
        from_type: None,
        line,
        byte,
    };
    if looks_like_path(token) {
        let path = token.trim_start_matches("./");
        let path = path.split(':').next().unwrap_or(path);
        return Some(reference(path, ReferenceKind::FileMention, None));
    }
    if !looks_like_code(token) {
        return None;
    }
    let separator = if token.contains("::") { "::" } else { "." };
    let (qualifier, name) = match token.rsplit_once(separator) {
        Some((qualifier, name)) => (Some(qualifier.replace('.', "::")), name),
        None => (None, token),
    };
    let identifier = |text: &str| {
        text.chars()
            .next()
            .is_some_and(|first| first.is_alphabetic() || first == '_')
            && text
                .chars()
                .all(|character| character.is_alphanumeric() || character == '_')
    };
    identifier(name).then(|| reference(name, ReferenceKind::Mention, qualifier))
}

fn looks_like_path(token: &str) -> bool {
    token.contains('/')
        && !token.contains("://")
        && token
            .rsplit('/')
            .next()
            .is_some_and(|last| last.contains('.') && !last.ends_with('.'))
}

fn looks_like_code(token: &str) -> bool {
    let camel =
        token.chars().skip(1).any(char::is_uppercase) && token.chars().any(char::is_lowercase);
    token.len() >= 3 && (token.contains('_') || token.contains("::") || camel)
}
