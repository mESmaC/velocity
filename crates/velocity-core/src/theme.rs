//! The theme module: the design tokens from `:root` in `design/main-window.html`
//! (SPEC.md §3, §6).
//!
//! `ui/src/lib/theme/tokens.css` is the one copy of the tokens. The UI loads it
//! as a stylesheet; this module embeds the same file at compile time and reads
//! its declarations, so the Rust side mirrors the UI by construction instead of
//! keeping a second hand-written list that could drift.
//!
//! The scanner is the design proven in `proofs/Theme/Design.lean`: one pass,
//! one byte at a time, never stopping early. Everything here is written in the
//! subset Charon and Aeneas translate (plain loops, slices, no closures or lazy
//! statics), so `proofs/Theme/Confidence.lean` covers the code itself. Only
//! `TOKENS_CSS` and `tokens()` are left out of the translation (see
//! `[package.metadata.charon]` in `Cargo.toml`): the embedded file is too large
//! a literal for Lean, and `tokens()` only passes it to `scan_token_spans`.
//!
//! It works on bytes rather than chars: every byte it reacts to (`-`, `/`, `*`,
//! `:`, ` `, `;`) is ASCII, and in UTF-8 an ASCII byte never occurs inside a
//! multi-byte character, so every name and value span is valid UTF-8 and
//! `std::str::from_utf8` on it always succeeds.

/// The token stylesheet, embedded when the crate is built.
pub const TOKENS_CSS: &[u8] = include_bytes!("../../../ui/src/lib/theme/tokens.css");

/// Byte offsets of one `--name: value;` declaration: the name without the
/// leading `--`, and the value without the padding before it.
#[derive(Clone, Copy)]
pub struct TokenSpan {
    pub name_start: usize,
    pub name_end: usize,
    pub value_start: usize,
    pub value_end: usize,
}

/// Where the scanner is. Mirrors `Mode` in `proofs/Theme/Design.lean`, with
/// byte offsets in place of the accumulated characters.
#[derive(Clone, Copy)]
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

fn declaration_step(
    mode: ScanMode,
    byte: u8,
    index: usize,
    spans: &mut Vec<TokenSpan>,
) -> ScanMode {
    match mode {
        ScanMode::Name { name_start } if byte == b':' => ScanMode::Padding {
            name_start,
            name_end: index,
        },
        ScanMode::Padding { .. } if byte == b' ' => mode,
        ScanMode::Padding {
            name_start,
            name_end,
        } if byte == b';' => {
            spans.push(TokenSpan {
                name_start,
                name_end,
                value_start: index,
                value_end: index,
            });
            ScanMode::Outside
        }
        ScanMode::Padding {
            name_start,
            name_end,
        } => ScanMode::Value {
            name_start,
            name_end,
            value_start: index,
        },
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
        _ => mode,
    }
}

/// Reads the byte at `index`. A finished declaration is pushed onto `spans`.
pub fn scan_step(mode: ScanMode, byte: u8, index: usize, spans: &mut Vec<TokenSpan>) -> ScanMode {
    match mode {
        ScanMode::Outside => outside_step(byte),
        ScanMode::Dash if byte == b'-' => ScanMode::Name {
            name_start: index + 1,
        },
        ScanMode::Dash => outside_step(byte),
        ScanMode::Slash | ScanMode::Comment | ScanMode::CommentStar => comment_step(mode, byte),
        _ => declaration_step(mode, byte, index, spans),
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

/// Every design token, in the order `tokens.css` declares them, as spans of
/// `TOKENS_CSS`: `&TOKENS_CSS[span.name_start..span.name_end]` is the name
/// without `--`, and `&TOKENS_CSS[span.value_start..span.value_end]` its value.
pub fn tokens() -> Vec<TokenSpan> {
    scan_token_spans(TOKENS_CSS)
}
