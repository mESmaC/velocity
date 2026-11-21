//! Markdown read for structure only (F19: no model in the index): headings become sections,
//! and code names in backticks become `mentions` from the section to the code.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::extract::{Definition, FileParse, Reference, ReferenceKind, SymbolKind};
use crate::text::{digest, first_line_summary};

const MAX_DIFF_BYTES: usize = 1024 * 1024;
const MAX_LCS_CELLS: usize = 160_000;
const MAX_CHANGED_SYMBOLS: usize = 12;
const MAX_ROW_KEY_CHARS: usize = 48;

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
    let mut definitions = sections(source, &headings);
    definitions.extend(plan_items(source, &headings));
    definitions.sort_by_key(|definition| {
        (
            definition.start_byte,
            u8::from(definition.kind != SymbolKind::Heading),
        )
    });
    FileParse {
        definitions,
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
                trait_name: None,
                command: false,
                test: false,
                start_line: heading.line,
                end_line: end_line.max(heading.line),
                start_byte: heading.byte as u32,
                end_byte: end_byte as u32,
                name_byte: heading.byte as u32,
                signature: format!("{} {}", "#".repeat(heading.level as usize), heading.title),
                doc: first_line_summary(&lines, heading.line as usize),
                span_hash: digest(span),
                level: heading.level,
                done: None,
            }
        })
        .collect()
}

/// Checklist items (`- [ ]` / `- [x]`) under a §7 Implementation order/plan heading become
/// individual records so agents can find, read and claim items without loading all of §7.
fn plan_items(source: &str, headings: &[Heading]) -> Vec<Definition> {
    let mut items = Vec::new();
    for (index, heading) in headings.iter().enumerate() {
        if !is_plan_heading(&heading.title) {
            continue;
        }
        let end_byte = headings[index + 1..]
            .iter()
            .find(|later| later.level <= heading.level)
            .map_or(source.len(), |later| later.byte);
        items.extend(extract_plan_items(source, heading, end_byte));
    }
    items
}

fn is_plan_heading(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    lower.contains("implementation order") || lower.contains("implementation plan")
}

fn extract_plan_items(source: &str, heading: &Heading, end_byte: usize) -> Vec<Definition> {
    let mut items = Vec::new();
    let mut group = String::new();
    let mut fenced = false;
    let mut byte = 0usize;
    for (index, line) in source.split_inclusive('\n').enumerate() {
        let line_end = byte + line.len();
        let in_section = byte > heading.byte && byte < end_byte;
        let trimmed = line.trim();
        let number = index as u32 + 1;
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
        } else if !fenced && in_section {
            if let Some(next_group) = group_heading(trimmed) {
                group = next_group;
            } else if let Some((done, title)) = checklist_item(trimmed) {
                let raw = line.trim_end_matches(['\r', '\n']);
                items.push(Definition {
                    name: title.to_owned(),
                    kind: SymbolKind::Item,
                    container: (!group.is_empty()).then(|| group.clone()),
                    trait_name: None,
                    command: false,
                    test: false,
                    start_line: number,
                    end_line: number,
                    start_byte: byte as u32,
                    end_byte: (byte + raw.len()) as u32,
                    name_byte: byte as u32,
                    signature: if done {
                        "[done]".to_owned()
                    } else {
                        "[open]".to_owned()
                    },
                    doc: String::new(),
                    span_hash: digest(raw.as_bytes()),
                    level: 0,
                    done: Some(done),
                });
            }
        }
        byte = line_end;
        if byte >= end_byte {
            break;
        }
    }
    items
}

fn group_heading(trimmed: &str) -> Option<String> {
    if let Some(rest) = trimmed.strip_prefix("**") {
        let title = rest.split_once("**").map_or(rest, |(bold, _)| bold).trim();
        return (!title.is_empty()).then(|| title.to_owned());
    }
    let level = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if level > 0 && level <= 6 && trimmed.as_bytes().get(level) == Some(&b' ') {
        let title = trimmed[level..].trim().trim_end_matches('#').trim();
        return (!title.is_empty()).then(|| title.to_owned());
    }
    None
}

fn checklist_item(trimmed: &str) -> Option<(bool, &str)> {
    let (done, rest) = if let Some(rest) = trimmed
        .strip_prefix("- [ ] ")
        .or_else(|| trimmed.strip_prefix("* [ ] "))
    {
        (false, rest)
    } else {
        let rest = trimmed
            .strip_prefix("- [x] ")
            .or_else(|| trimmed.strip_prefix("- [X] "))
            .or_else(|| trimmed.strip_prefix("* [x] "))
            .or_else(|| trimmed.strip_prefix("* [X] "))?;
        (true, rest)
    };
    let title = rest.trim();
    (!title.is_empty()).then_some((done, title))
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum LineKind {
    Heading(String),
    TableHeader,
    TableRow { key: String },
    Content,
    Blank,
}

#[derive(Clone, Debug)]
struct MarkdownLine {
    heading: Option<String>,
    kind: LineKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DiffHunk {
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
}

/// Finer Markdown symbols for overlap detection (#564): table rows keyed by innermost
/// heading and first cell, edited heading lines, and base line ranges for prose/lists.
pub(crate) fn changed_symbols(before: Option<&str>, after: Option<&str>) -> Vec<String> {
    let before = before.filter(|text| text.len() <= MAX_DIFF_BYTES);
    let after = after.filter(|text| text.len() <= MAX_DIFF_BYTES);
    if before == after {
        return Vec::new();
    }
    let mut symbols = BTreeSet::new();
    match (before, after) {
        (None, None) => {}
        (Some(text), None) | (None, Some(text)) => {
            let lines: Vec<&str> = text.lines().collect();
            let units = classify_lines(&lines);
            collect_range_symbols(&units, 0, units.len(), &mut symbols);
        }
        (Some(old_text), Some(new_text)) => {
            let old_lines: Vec<&str> = old_text.lines().collect();
            let new_lines: Vec<&str> = new_text.lines().collect();
            let before_units = classify_lines(&old_lines);
            let after_units = classify_lines(&new_lines);
            let mut hunks = Vec::new();
            diff_range(&old_lines, &new_lines, 0, 0, &mut hunks);
            for hunk in hunks {
                collect_hunk_symbols(hunk, &before_units, &after_units, &mut symbols);
            }
        }
    }
    symbols.into_iter().take(MAX_CHANGED_SYMBOLS).collect()
}

/// Returns the intersecting range label when two Markdown line-range symbols overlap
/// within the same heading (for example `State:L10-12` and `State:L11-13` -> `State:L11-12`).
pub(crate) fn overlapping_line_range(left: &str, right: &str) -> Option<String> {
    let (left_heading, left_start, left_end) = parse_line_range(left)?;
    let (right_heading, right_start, right_end) = parse_line_range(right)?;
    if left_heading != right_heading {
        return None;
    }
    let start = left_start.max(right_start);
    let end = left_end.min(right_end);
    (start <= end).then(|| format_line_range(left_heading, start, end))
}

fn parse_line_range(symbol: &str) -> Option<(Option<&str>, u32, u32)> {
    let (heading, range) = match symbol.rsplit_once(":L") {
        Some((heading, range)) => (Some(heading), range),
        None => (None, symbol.strip_prefix('L')?),
    };
    let (start, end) = match range.split_once('-') {
        Some((start, end)) => (start.parse::<u32>().ok()?, end.parse::<u32>().ok()?),
        None => {
            let line = range.parse::<u32>().ok()?;
            (line, line)
        }
    };
    (start > 0 && start <= end).then_some((heading, start, end))
}

fn classify_lines(lines: &[&str]) -> Vec<MarkdownLine> {
    let mut units = Vec::with_capacity(lines.len());
    let mut current_heading: Option<String> = None;
    let mut fence: Option<(char, usize)> = None;
    let mut index = 0usize;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        let kind = if let Some((fence_char, fence_len)) = fence {
            if is_fence_close(trimmed, fence_char, fence_len) {
                fence = None;
            }
            LineKind::Content
        } else if let Some(opened) = fence_opener(trimmed) {
            fence = Some(opened);
            LineKind::Content
        } else if trimmed.is_empty() {
            LineKind::Blank
        } else if let Some(parsed) = heading(trimmed, index as u32 + 1, 0) {
            current_heading = Some(parsed.title.clone());
            units.push(MarkdownLine {
                heading: None,
                kind: LineKind::Heading(parsed.title),
            });
            index += 1;
            continue;
        } else if trimmed.contains('|')
            && lines
                .get(index + 1)
                .is_some_and(|next| is_table_separator(next))
        {
            index = classify_table(lines, index, current_heading.as_deref(), &mut units);
            continue;
        } else {
            LineKind::Content
        };
        units.push(MarkdownLine {
            heading: current_heading.clone(),
            kind,
        });
        index += 1;
    }
    units
}

fn classify_table(
    lines: &[&str],
    start: usize,
    current_heading: Option<&str>,
    units: &mut Vec<MarkdownLine>,
) -> usize {
    let heading_owned = current_heading.map(str::to_owned);
    units.push(MarkdownLine {
        heading: heading_owned.clone(),
        kind: LineKind::TableHeader,
    });
    units.push(MarkdownLine {
        heading: heading_owned.clone(),
        kind: LineKind::TableHeader,
    });
    let mut index = start + 2;
    let mut seen_keys: BTreeMap<String, usize> = BTreeMap::new();
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed.is_empty()
            || !trimmed.contains('|')
            || heading(trimmed, index as u32 + 1, 0).is_some()
            || fence_opener(trimmed).is_some()
        {
            break;
        }
        let base_key = table_row_key(trimmed).unwrap_or_else(|| format!("L{}", index + 1));
        let count = seen_keys.entry(base_key.clone()).or_insert(0);
        *count += 1;
        let key = if *count == 1 {
            base_key
        } else {
            format!("{base_key}#{count}")
        };
        units.push(MarkdownLine {
            heading: heading_owned.clone(),
            kind: LineKind::TableRow { key },
        });
        index += 1;
    }
    index
}

fn fence_opener(trimmed: &str) -> Option<(char, usize)> {
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let count = trimmed
        .chars()
        .take_while(|character| *character == first)
        .count();
    (count >= 3).then_some((first, count))
}

fn is_fence_close(trimmed: &str, fence_char: char, min_len: usize) -> bool {
    let count = trimmed
        .chars()
        .take_while(|character| *character == fence_char)
        .count();
    count >= min_len && trimmed.trim_start_matches(fence_char).trim().is_empty()
}

fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.contains('|') || !trimmed.contains('-') {
        return false;
    }
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = 0usize;
    for cell in inner.split('|') {
        let cell = cell.trim();
        if cell.is_empty()
            || !cell.contains('-')
            || !cell.bytes().all(|byte| matches!(byte, b'-' | b':'))
        {
            return false;
        }
        cells += 1;
    }
    cells > 0
}

fn table_row_key(trimmed: &str) -> Option<String> {
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let cell = inner.split('|').next()?.trim();
    let cleaned = cell
        .trim_matches(|character| matches!(character, '`' | '*' | '_'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        return None;
    }
    Some(cleaned.chars().take(MAX_ROW_KEY_CHARS).collect())
}

fn collect_hunk_symbols(
    hunk: DiffHunk,
    before: &[MarkdownLine],
    after: &[MarkdownLine],
    symbols: &mut BTreeSet<String>,
) {
    let mut recorded = false;
    if hunk.old_start < hunk.old_end {
        recorded |= collect_range_symbols(before, hunk.old_start, hunk.old_end, symbols);
        recorded |= collect_named_units(after, hunk.new_start, hunk.new_end, symbols);
    } else if hunk.new_start < hunk.new_end {
        recorded |= collect_named_units(after, hunk.new_start, hunk.new_end, symbols);
        let inserted_content = after[hunk.new_start..hunk.new_end]
            .iter()
            .find(|line| line.kind == LineKind::Content);
        if let Some(content_line) = inserted_content {
            let heading = content_line
                .heading
                .as_deref()
                .or_else(|| base_heading_near(before, hunk.old_start));
            let anchor = (hunk.old_start + 1) as u32;
            symbols.insert(format_line_range(heading, anchor, anchor));
            recorded = true;
        }
    }
    if !recorded {
        let heading = base_heading_near(before, hunk.old_start);
        let start = (hunk.old_start + 1) as u32;
        let end = hunk.old_end.max(hunk.old_start + 1) as u32;
        symbols.insert(format_line_range(heading, start, end));
    }
}

fn collect_range_symbols(
    units: &[MarkdownLine],
    start: usize,
    end: usize,
    symbols: &mut BTreeSet<String>,
) -> bool {
    let mut recorded = false;
    let mut content_run: Option<(Option<&str>, u32, u32)> = None;
    let flush = |run: &mut Option<(Option<&str>, u32, u32)>,
                 symbols: &mut BTreeSet<String>,
                 recorded: &mut bool| {
        if let Some((heading, run_start, run_end)) = run.take() {
            symbols.insert(format_line_range(heading, run_start, run_end));
            *recorded = true;
        }
    };
    for (offset, unit) in units[start..end].iter().enumerate() {
        let line_number = (start + offset + 1) as u32;
        match &unit.kind {
            LineKind::Heading(title) => {
                flush(&mut content_run, symbols, &mut recorded);
                symbols.insert(title.clone());
                recorded = true;
            }
            LineKind::TableHeader => {
                flush(&mut content_run, symbols, &mut recorded);
                symbols.insert(format_scoped(unit.heading.as_deref(), "table-header"));
                recorded = true;
            }
            LineKind::TableRow { key } => {
                flush(&mut content_run, symbols, &mut recorded);
                symbols.insert(format_scoped(
                    unit.heading.as_deref(),
                    &format!("row:{key}"),
                ));
                recorded = true;
            }
            LineKind::Content => {
                let heading = unit.heading.as_deref();
                match &mut content_run {
                    Some((run_heading, _, run_end))
                        if *run_heading == heading && *run_end + 1 == line_number =>
                    {
                        *run_end = line_number;
                    }
                    _ => {
                        flush(&mut content_run, symbols, &mut recorded);
                        content_run = Some((heading, line_number, line_number));
                    }
                }
            }
            LineKind::Blank => flush(&mut content_run, symbols, &mut recorded),
        }
    }
    flush(&mut content_run, symbols, &mut recorded);
    recorded
}

fn collect_named_units(
    units: &[MarkdownLine],
    start: usize,
    end: usize,
    symbols: &mut BTreeSet<String>,
) -> bool {
    let mut recorded = false;
    for unit in &units[start..end] {
        match &unit.kind {
            LineKind::Heading(title) => {
                symbols.insert(title.clone());
                recorded = true;
            }
            LineKind::TableHeader => {
                symbols.insert(format_scoped(unit.heading.as_deref(), "table-header"));
                recorded = true;
            }
            LineKind::TableRow { key } => {
                symbols.insert(format_scoped(
                    unit.heading.as_deref(),
                    &format!("row:{key}"),
                ));
                recorded = true;
            }
            LineKind::Content | LineKind::Blank => {}
        }
    }
    recorded
}

fn base_heading_near(before: &[MarkdownLine], old_start: usize) -> Option<&str> {
    let candidate = before
        .get(old_start)
        .or_else(|| old_start.checked_sub(1).and_then(|idx| before.get(idx)))?;
    match &candidate.kind {
        LineKind::Heading(title) => Some(title.as_str()),
        _ => candidate.heading.as_deref(),
    }
}

fn format_scoped(heading: Option<&str>, label: &str) -> String {
    match heading {
        Some(title) if !title.is_empty() => format!("{title}::{label}"),
        _ => label.to_owned(),
    }
}

fn format_line_range(heading: Option<&str>, start: u32, end: u32) -> String {
    let range = if start == end {
        format!("L{start}")
    } else {
        format!("L{start}-{end}")
    };
    match heading {
        Some(title) if !title.is_empty() => format!("{title}:{range}"),
        _ => range,
    }
}

fn trim_common_edges(old: &[&str], new: &[&str]) -> (usize, usize) {
    let mut prefix = 0usize;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while prefix + suffix < old.len()
        && prefix + suffix < new.len()
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    (prefix, suffix)
}

fn diff_range(
    old: &[&str],
    new: &[&str],
    old_offset: usize,
    new_offset: usize,
    out: &mut Vec<DiffHunk>,
) {
    let (prefix, suffix) = trim_common_edges(old, new);
    let old_mid = &old[prefix..old.len() - suffix];
    let new_mid = &new[prefix..new.len() - suffix];
    let base_old = old_offset + prefix;
    let base_new = new_offset + prefix;
    if old_mid.is_empty() && new_mid.is_empty() {
        return;
    }
    if old_mid.is_empty() || new_mid.is_empty() {
        out.push(DiffHunk {
            old_start: base_old,
            old_end: base_old + old_mid.len(),
            new_start: base_new,
            new_end: base_new + new_mid.len(),
        });
        return;
    }
    if old_mid.len().saturating_mul(new_mid.len()) <= MAX_LCS_CELLS {
        lcs_hunks(old_mid, new_mid, base_old, base_new, out);
        return;
    }
    let anchors = unique_anchors(old_mid, new_mid);
    if anchors.is_empty() {
        out.push(DiffHunk {
            old_start: base_old,
            old_end: base_old + old_mid.len(),
            new_start: base_new,
            new_end: base_new + new_mid.len(),
        });
        return;
    }
    let (mut prev_old, mut prev_new) = (0usize, 0usize);
    for (anchor_old, anchor_new) in anchors {
        diff_range(
            &old_mid[prev_old..anchor_old],
            &new_mid[prev_new..anchor_new],
            base_old + prev_old,
            base_new + prev_new,
            out,
        );
        prev_old = anchor_old + 1;
        prev_new = anchor_new + 1;
    }
    diff_range(
        &old_mid[prev_old..],
        &new_mid[prev_new..],
        base_old + prev_old,
        base_new + prev_new,
        out,
    );
}

fn lcs_hunks(
    old: &[&str],
    new: &[&str],
    base_old: usize,
    base_new: usize,
    out: &mut Vec<DiffHunk>,
) {
    let m = old.len();
    let n = new.len();
    let width = n + 1;
    let mut table = vec![0u32; (m + 1) * width];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            table[i * width + j] = if old[i] == new[j] {
                table[(i + 1) * width + (j + 1)] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + (j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    while i < m || j < n {
        if i < m && j < n && old[i] == new[j] {
            i += 1;
            j += 1;
            continue;
        }
        let hunk_old = i;
        let hunk_new = j;
        while i < m || j < n {
            if i < m && j < n && old[i] == new[j] {
                break;
            }
            if i < m && (j == n || table[(i + 1) * width + j] >= table[i * width + (j + 1)]) {
                i += 1;
            } else {
                j += 1;
            }
        }
        out.push(DiffHunk {
            old_start: base_old + hunk_old,
            old_end: base_old + i,
            new_start: base_new + hunk_new,
            new_end: base_new + j,
        });
    }
}

fn unique_anchors(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    let mut old_counts: HashMap<&str, usize> = HashMap::new();
    for line in old {
        *old_counts.entry(*line).or_insert(0) += 1;
    }
    let mut new_positions: HashMap<&str, Option<usize>> = HashMap::new();
    for (index, line) in new.iter().enumerate() {
        new_positions
            .entry(*line)
            .and_modify(|slot| *slot = None)
            .or_insert(Some(index));
    }
    let mut pairs = Vec::new();
    for (old_index, line) in old.iter().enumerate() {
        if old_counts.get(line) == Some(&1)
            && let Some(Some(new_index)) = new_positions.get(line)
        {
            pairs.push((old_index, *new_index));
        }
    }
    longest_increasing_anchors(&pairs)
}

fn longest_increasing_anchors(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if pairs.is_empty() {
        return Vec::new();
    }
    let mut tails: Vec<usize> = Vec::new();
    let mut previous = vec![None; pairs.len()];
    for (index, &(_, new_index)) in pairs.iter().enumerate() {
        let slot = tails.partition_point(|&tail| pairs[tail].1 < new_index);
        if slot > 0 {
            previous[index] = Some(tails[slot - 1]);
        }
        if slot == tails.len() {
            tails.push(index);
        } else {
            tails[slot] = index;
        }
    }
    let mut chain = Vec::new();
    let mut cursor = tails.last().copied();
    while let Some(index) = cursor {
        chain.push(pairs[index]);
        cursor = previous[index];
    }
    chain.reverse();
    chain
}

#[cfg(test)]
mod tests {
    use super::{changed_symbols, overlapping_line_range};

    // Risk (#564): editing different rows of the same Markdown table previously hashed the
    // enclosing heading section and caused symmetric shared-symbol overlap deadlocks.
    #[test]
    fn different_table_rows_do_not_share_symbols() {
        let base = "\
# Screens

## Which pages v0 needs

| Page | Route | Status |
| --- | --- | --- |
| Home | `/` | planned |
| ChangeView | `/changes/:id` | planned |
| Settings | `/settings` | planned |
";
        let edit_home = base.replace("| Home | `/` | planned |", "| Home | `/` | shipped |");
        let edit_change = base.replace(
            "| ChangeView | `/changes/:id` | planned |",
            "| ChangeView | `/changes/:id` | shipped |",
        );
        let symbols_home = changed_symbols(Some(base), Some(&edit_home));
        let symbols_change = changed_symbols(Some(base), Some(&edit_change));
        assert_eq!(symbols_home, vec!["Which pages v0 needs::row:Home"]);
        assert_eq!(symbols_change, vec!["Which pages v0 needs::row:ChangeView"]);
    }

    // Risk (#564): editing different prose lines in one Markdown section must produce disjoint
    // line-range symbols while overlapping ranges still intersect.
    #[test]
    fn prose_edits_use_base_line_ranges() {
        let base = "\
# C12 Linked server state

## Reconnect

First sentence on line 5.
Second sentence on line 6.
Third sentence on line 7.
Fourth sentence on line 8.
";
        let edit_first = base.replace("First sentence on line 5.", "Updated first sentence.");
        let edit_fourth = base.replace("Fourth sentence on line 8.", "Updated fourth sentence.");
        let first_symbols = changed_symbols(Some(base), Some(&edit_first));
        let fourth_symbols = changed_symbols(Some(base), Some(&edit_fourth));
        assert_eq!(first_symbols, vec!["Reconnect:L5"]);
        assert_eq!(fourth_symbols, vec!["Reconnect:L8"]);
        assert_eq!(
            overlapping_line_range("Reconnect:L5-7", "Reconnect:L6-8").as_deref(),
            Some("Reconnect:L6-7")
        );
        assert_eq!(overlapping_line_range("Reconnect:L5", "Reconnect:L8"), None);
    }
}
