//! The theme module: the design tokens from `:root` in `design/main-window.html`
//! (SPEC.md §3, §6).
//!
//! `ui/src/lib/theme/tokens.css` is the one copy of the tokens. The UI loads it
//! as a stylesheet; this module embeds the same file at compile time and reads
//! its declarations, so the Rust side mirrors the UI by construction instead of
//! keeping a second hand-written list that could drift.
//!
//! The scanner is the design proven in `proofs/Theme/Design.lean`: one pass,
//! one byte at a time, never stopping early. It works on bytes rather than
//! chars because every byte it reacts to (`-`, `/`, `*`, `:`, ` `, `;`) is ASCII,
//! and in UTF-8 an ASCII byte never occurs inside a multi-byte character, so
//! every span it returns starts and ends on a character boundary.

use std::sync::LazyLock;

/// The token stylesheet, embedded when the crate is built.
pub const TOKENS_CSS: &str = include_str!("../../../ui/src/lib/theme/tokens.css");

/// One design token: `--name: value;` gives `name` without the leading `--`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub name: &'static str,
    pub value: &'static str,
}

/// Byte offsets of one declaration's name and value in the scanned text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenSpan {
    pub name_start: usize,
    pub name_end: usize,
    pub value_start: usize,
    pub value_end: usize,
}

/// Where the scanner is. Mirrors `Mode` in `proofs/Theme/Design.lean`, with
/// byte offsets in place of the accumulated characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
    Outside,
    /// Read one `-` outside a declaration.
    Dash,
    /// Read one `/` outside a comment.
    Slash,
    Comment,
    /// Inside a comment, just after a `*`.
    CommentStar,
    Name {
        name_start: usize,
    },
    /// Between `:` and the first byte of the value.
    Padding {
        name_start: usize,
        name_end: usize,
    },
    Value {
        name_start: usize,
        name_end: usize,
        value_start: usize,
    },
}

fn outside_step(byte: u8) -> ScanMode {
    if byte == b'-' {
        ScanMode::Dash
    } else if byte == b'/' {
        ScanMode::Slash
    } else {
        ScanMode::Outside
    }
}

fn comment_step(mode: ScanMode, byte: u8) -> ScanMode {
    match mode {
        ScanMode::Slash if byte == b'*' => ScanMode::Comment,
        ScanMode::Slash => outside_step(byte),
        ScanMode::CommentStar if byte == b'/' => ScanMode::Outside,
        ScanMode::Comment | ScanMode::CommentStar if byte == b'*' => ScanMode::CommentStar,
        _ => ScanMode::Comment,
    }
}

/// Reads one byte at `index`. A finished declaration is pushed onto `spans`.
pub fn scan_step(mode: ScanMode, byte: u8, index: usize, spans: &mut Vec<TokenSpan>) -> ScanMode {
    match mode {
        ScanMode::Outside => outside_step(byte),
        ScanMode::Dash if byte == b'-' => ScanMode::Name {
            name_start: index + 1,
        },
        ScanMode::Dash => outside_step(byte),
        ScanMode::Slash | ScanMode::Comment | ScanMode::CommentStar => comment_step(mode, byte),
        ScanMode::Name { name_start } if byte == b':' => ScanMode::Padding {
            name_start,
            name_end: index,
        },
        ScanMode::Name { .. } => mode,
        ScanMode::Padding { .. } if byte == b' ' => mode,
        ScanMode::Padding {
            name_start,
            name_end,
        } => {
            let value_start = index;
            if byte == b';' {
                spans.push(TokenSpan {
                    name_start,
                    name_end,
                    value_start,
                    value_end: index,
                });
                ScanMode::Outside
            } else {
                ScanMode::Value {
                    name_start,
                    name_end,
                    value_start,
                }
            }
        }
        ScanMode::Value {
            name_start,
            name_end,
            value_start,
        } if byte == b';' => {
            spans.push(TokenSpan {
                name_start,
                name_end,
                value_start,
                value_end: index,
            });
            ScanMode::Outside
        }
        ScanMode::Value { .. } => mode,
    }
}

/// The spans of every `--name: value;` declaration in `css`, in order.
pub fn scan_token_spans(css: &[u8]) -> Vec<TokenSpan> {
    let mut spans = Vec::new();
    let mut mode = ScanMode::Outside;
    let mut index = 0;
    while index < css.len() {
        mode = scan_step(mode, css[index], index, &mut spans);
        index += 1;
    }
    spans
}

/// Every declaration in `css`, in order.
pub fn parse_tokens(css: &'static str) -> Vec<Token> {
    scan_token_spans(css.as_bytes())
        .into_iter()
        .filter_map(|span| {
            // Spans always fall on character boundaries (see the module
            // comment), so `get` never returns `None` here.
            let name = css.get(span.name_start..span.name_end)?;
            let value = css.get(span.value_start..span.value_end)?;
            Some(Token { name, value })
        })
        .collect()
}

static TOKENS: LazyLock<Vec<Token>> = LazyLock::new(|| parse_tokens(TOKENS_CSS));

/// Every design token, in the order `tokens.css` declares them.
pub fn tokens() -> &'static [Token] {
    &TOKENS
}

/// The value of the token `--name`, given `name` without the leading `--`.
pub fn token(name: &str) -> Option<&'static str> {
    tokens()
        .iter()
        .find(|token| token.name == name)
        .map(|token| token.value)
}
