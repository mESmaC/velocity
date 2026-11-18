//! UTF-8 budgets for text Velocity puts in agent context.

pub const TOOL_ANSWER_BYTES: usize = 8_000;
pub const SHORT_PROMPT_BYTES: usize = 4_000;
pub const PROMPT_BYTES: usize = 8_000;

pub fn prefix(text: &str, bytes: usize) -> &str {
    &text[..text.floor_char_boundary(bytes)]
}

pub fn preview(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_owned();
    }
    let marker = "…";
    if bytes < marker.len() {
        return prefix(text, bytes).to_owned();
    }
    format!("{}{marker}", prefix(text, bytes - marker.len()))
}

/// Leave a readable result even if a tool without pagination returns oversized JSON.
pub fn tool_answer(text: String, hint: &str) -> String {
    if text.len() <= TOOL_ANSWER_BYTES {
        return text;
    }
    let notice = format!("[Answer clipped to 8,000 characters. {hint}]\n");
    let excerpt = prefix(&text, TOOL_ANSWER_BYTES - notice.len());
    format!("{notice}{excerpt}")
}

pub fn prompt_excerpt(text: &str, budget: usize, notice: &str) -> String {
    if text.len() <= budget {
        return text.to_owned();
    }
    let notice = prefix(notice, budget);
    format!("{}{notice}", prefix(text, budget - notice.len()))
}

/// Page a string by UTF-8 byte offset, allowing for escaping inside a JSON response.
pub fn json_text_page(text: &str, offset: usize, budget: usize) -> Option<(&str, Option<usize>)> {
    let remaining = text.get(offset..)?;
    let mut bytes = 0;
    let mut end = remaining.len();
    for (position, character) in remaining.char_indices() {
        bytes += match character {
            '\\' | '"' | '\n' | '\r' | '\t' => 2,
            '\0'..='\u{001f}' => 6,
            _ => character.len_utf8(),
        };
        if bytes > budget {
            end = position;
            break;
        }
    }
    if end < remaining.len()
        && let Some(line_end) = remaining[..end].rfind('\n')
        && line_end >= end / 2
    {
        end = line_end + 1;
    }
    let next = offset + end;
    Some((&remaining[..end], (next < text.len()).then_some(next)))
}
