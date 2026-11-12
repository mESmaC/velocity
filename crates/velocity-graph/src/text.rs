//! Small text helpers shared by the extractors: signatures, doc summaries, hashes and names.

use sha2::{Digest, Sha256};

const SIGNATURE_LIMIT: usize = 200;
const DOC_LIMIT: usize = 200;

/// A hex digest of `bytes`, 32 characters (128 bits): the parse cache key and span hashes.
pub fn digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    hash.iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A definition's signature: its text up to the start of its body, on one line.
pub fn signature(source: &str, start: usize, end: usize) -> String {
    let text = source.get(start..end.min(source.len())).unwrap_or_default();
    let head = text
        .find(['{', '\n'])
        .map(|stop| {
            // A signature that spans lines (Rust parameters, TypeScript generics) keeps going
            // to its body's brace, or stops at the first line that ends a declaration.
            if text[stop..].starts_with('\n') {
                let brace = text.find('{').unwrap_or(text.len());
                let semicolon = text.find(';').map_or(text.len(), |at| at + 1);
                let arrow = text.find("=>").map_or(text.len(), |at| at + 2);
                &text[..brace.min(semicolon).min(arrow).min(600)]
            } else {
                &text[..stop]
            }
        })
        .unwrap_or(text);
    shorten(&collapse(head), SIGNATURE_LIMIT)
}

/// The first sentence of the doc comment above `start_line` (1-based), or of a Python
/// docstring just below it.
pub fn doc_summary(lines: &[&str], start_line: usize) -> String {
    let mut collected = Vec::new();
    let mut index = start_line.saturating_sub(1);
    while index > 0 {
        index -= 1;
        let line = lines.get(index).map_or("", |line| line.trim());
        if line.starts_with("#[") || line.starts_with('@') {
            continue;
        }
        let Some(body) = comment_body(line) else {
            break;
        };
        collected.push(body);
    }
    collected.reverse();
    let mut text = collapse(&collected.join(" "));
    if text.is_empty() {
        text = docstring(lines, start_line);
    }
    first_sentence(&text)
}

fn comment_body(line: &str) -> Option<&str> {
    ["///", "//!", "//", "/**", "/*", "*/", "*", "#"]
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix))
        .map(|body| body.trim_end_matches("*/").trim())
}

fn docstring(lines: &[&str], start_line: usize) -> String {
    let next = lines.get(start_line).map_or("", |line| line.trim());
    for quote in ["\"\"\"", "'''"] {
        if let Some(body) = next.strip_prefix(quote) {
            return collapse(body.split(quote).next().unwrap_or_default());
        }
    }
    String::new()
}

/// The first sentence of the first paragraph after line `heading_line` (1-based).
pub fn first_line_summary(lines: &[&str], heading_line: usize) -> String {
    let paragraph = lines
        .iter()
        .skip(heading_line)
        .map(|line| line.trim())
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or_default();
    first_sentence(&collapse(
        paragraph.trim_start_matches(['-', '*', '|', '>', ' ']),
    ))
}

fn first_sentence(text: &str) -> String {
    let end = text
        .match_indices(". ")
        .next()
        .map_or(text.len(), |(at, _)| at + 1);
    shorten(&text[..end], DOC_LIMIT)
}

/// Whitespace runs collapsed to one space.
pub fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` cut to at most `limit` characters, ending in `…` when cut.
pub fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(limit.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The type a qualifier or impl names: `&'a mut crate::x::Foo<T>` is `Foo`.
pub fn type_name(text: &str) -> String {
    let text = text.trim().trim_start_matches('&');
    let text = text
        .split_whitespace()
        .filter(|word| !word.starts_with('\'') && *word != "mut" && *word != "dyn")
        .collect::<Vec<_>>()
        .join(" ");
    let before_generics = text.split('<').next().unwrap_or_default();
    before_generics
        .rsplit("::")
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// A qualifier without generics or whitespace: `Vec::<u8>` is `Vec`, `a :: b` is `a::b`.
pub fn qualifier(text: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::new();
    for character in text.chars() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 && !character.is_whitespace() => out.push(character),
            _ => {}
        }
    }
    out.trim_end_matches("::").to_owned()
}
