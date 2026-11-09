//! Lossless line parsing for F11. Unrecognised Markdown stays editable only in its source file.

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutlineKind {
    Title,
    Section,
    Rule,
    Source,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct InlineText {
    pub text: String,
    pub code: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct OutlineLine {
    pub identifier: u32,
    pub kind: OutlineKind,
    pub text: String,
    pub nested: bool,
    pub inline: Vec<InlineText>,
    pub(crate) prefix: String,
    pub(crate) ending: String,
    pub(crate) continuation: String,
    pub(crate) placeholder: bool,
}

impl OutlineLine {
    pub(crate) fn new(identifier: u32, source: &str, literal: bool) -> Self {
        let (body, ending) = split_ending(source);
        let (kind, prefix, text, nested) = classify(body, literal);
        Self {
            identifier,
            kind,
            text: text.into(),
            nested,
            inline: inline_text(text),
            prefix: prefix.into(),
            ending: ending.into(),
            continuation: String::new(),
            placeholder: false,
        }
    }

    pub(crate) fn replace(&mut self, text: String) {
        // A pasted multiline rule remains a single bullet rather than creating hidden structure.
        self.text = text.replace('\r', "").replace('\n', " ");
        self.inline = inline_text(&self.text);
    }

    pub(crate) fn markdown(&self) -> String {
        if self.placeholder && self.text.trim().is_empty() && self.continuation.is_empty() {
            return String::new();
        }
        format!(
            "{}{}{}{}",
            self.prefix, self.text, self.ending, self.continuation
        )
    }
}

pub(crate) fn parse(source: &str) -> Vec<OutlineLine> {
    let mut lines: Vec<OutlineLine> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for (index, source_line) in source.split_inclusive('\n').enumerate() {
        let trimmed = source_line.trim_start();
        let marker = fence_marker(trimmed);
        let literal = fence.is_some() || marker.is_some();
        match (fence, marker) {
            (None, Some(marker)) => fence = Some(marker),
            (Some((character, count)), Some((end, length)))
                if character == end && length >= count =>
            {
                fence = None
            }
            _ => {}
        }
        let line = OutlineLine::new(
            u32::try_from(index).unwrap_or(u32::MAX),
            source_line,
            literal,
        );
        if line.kind == OutlineKind::Source
            && !literal
            && source_line.starts_with([' ', '\t'])
            && let Some(previous) = lines
                .last_mut()
                .filter(|line| line.kind == OutlineKind::Rule)
        {
            previous.continuation.push_str(source_line);
        } else {
            lines.push(line);
        }
    }
    lines
}

fn split_ending(source: &str) -> (&str, &str) {
    if let Some(body) = source.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = source.strip_suffix('\n') {
        (body, "\n")
    } else {
        (source, "")
    }
}

fn classify(body: &str, literal: bool) -> (OutlineKind, &str, &str, bool) {
    if !literal {
        if let Some(text) = body.strip_prefix("# ") {
            return (OutlineKind::Title, "# ", text, false);
        }
        if let Some(text) = body.strip_prefix("## ") {
            return (OutlineKind::Section, "## ", text, false);
        }
        let trimmed = body.trim_start_matches([' ', '\t']);
        let indentation = body.len() - trimmed.len();
        if indentation <= 2 {
            for marker in ["- ", "* ", "+ "] {
                if let Some(text) = trimmed.strip_prefix(marker) {
                    return (
                        OutlineKind::Rule,
                        &body[..indentation + 2],
                        text,
                        indentation > 0,
                    );
                }
            }
        }
    }
    (OutlineKind::Source, "", body, false)
}

fn fence_marker(source: &str) -> Option<(char, usize)> {
    let character = source.chars().next()?;
    let count = source.chars().take_while(|next| *next == character).count();
    (matches!(character, '`' | '~') && count >= 3).then_some((character, count))
}

fn inline_text(text: &str) -> Vec<InlineText> {
    // Unmatched backticks stay visible. The frontend creates text nodes, never HTML.
    let mut parts = Vec::new();
    let mut remaining = text;
    while let Some((prefix, after)) = remaining.split_once('`') {
        let Some((code, tail)) = after.split_once('`') else {
            break;
        };
        parts.push(InlineText {
            text: prefix.into(),
            code: false,
        });
        parts.push(InlineText {
            text: code.into(),
            code: true,
        });
        remaining = tail;
    }
    parts.push(InlineText {
        text: remaining.into(),
        code: false,
    });
    parts
}
