//! Spec section spans, §8 open questions, and §7 implementation-order item matching/ticking.

use std::path::{Path, PathBuf};

use crate::{ProjectSpec, headings, section_id, visible_lines};

/// One heading's location inside a project spec file (1-based inclusive lines).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionSpan {
    pub id: String,
    pub title: String,
    pub path: PathBuf,
    pub start_line: usize,
    pub end_line: usize,
}

/// One row from the §8 Open questions table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenQuestion {
    pub id: String,
    pub question: String,
    pub blocks: String,
    pub decided: bool,
    pub path: PathBuf,
    pub line: usize,
}

/// One checklist entry under §7 Implementation order (or Implementation plan).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImplementationItem {
    pub group: String,
    pub title: String,
    pub checked: bool,
    pub path: PathBuf,
    pub line: usize,
}

/// Criteria used to locate the owning §7 checkbox when a change lands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickQuery<'a> {
    pub spec_section: Option<&'a str>,
    pub issue_number: Option<u32>,
    pub title: Option<&'a str>,
}

impl TickQuery<'_> {
    pub fn is_empty(&self) -> bool {
        self.spec_section
            .is_none_or(|value| value.trim().is_empty())
            && self.issue_number.is_none()
            && self.title.is_none_or(|value| value.trim().is_empty())
    }
}

impl ProjectSpec {
    /// Finds the file and line span of the heading with `identifier` (e.g. `F9`, `§7`, `§3.1`).
    pub fn locate_section(&self, identifier: &str) -> Option<SectionSpan> {
        let wanted = normalize_section_id(identifier)?;
        for file in &self.files {
            if let Some(span) = locate_in_file(&file.path, &file.markdown, &wanted) {
                return Some(span);
            }
        }
        None
    }

    /// Parses every table row in the §8 Open questions section.
    pub fn open_questions(&self) -> Vec<OpenQuestion> {
        let mut questions = Vec::new();
        for file in &self.files {
            questions.extend(parse_open_questions(&file.path, &file.markdown));
        }
        questions
    }

    /// §8 rows that reference any of `sections` (token-matched) or feature `keywords`.
    pub fn matching_questions(
        &self,
        sections: &[String],
        keywords: &[String],
    ) -> Vec<OpenQuestion> {
        let normalized_sections: Vec<String> = sections
            .iter()
            .filter_map(|section| normalize_section_id(section))
            .collect();
        let lowered_keywords: Vec<String> = keywords
            .iter()
            .map(|word| word.trim().to_ascii_lowercase())
            .filter(|word| word.len() >= 4)
            .collect();
        if normalized_sections.is_empty() && lowered_keywords.is_empty() {
            return Vec::new();
        }
        self.open_questions()
            .into_iter()
            .filter(|row| question_matches(row, &normalized_sections, &lowered_keywords))
            .collect()
    }

    /// Parses all checklist items under `Implementation order` / `Implementation plan`.
    pub fn implementation_items(&self) -> Vec<ImplementationItem> {
        let mut items = Vec::new();
        for file in &self.files {
            items.extend(parse_implementation_items(&file.path, &file.markdown));
        }
        items
    }
}

/// Extracts unique spec section IDs (`F1`..`F99`, `§1`..`§8`, `§3.1`) referenced in `texts`.
pub fn extract_section_ids(texts: &[&str]) -> Vec<String> {
    let mut sections = Vec::new();
    for word in texts.iter().flat_map(|text| {
        text.split(|character: char| {
            !character.is_alphanumeric() && character != '.' && character != '§'
        })
    }) {
        if word.starts_with(['F', '§'])
            && let Some(section) = section_id(word)
            && !sections.contains(&section)
        {
            sections.push(section);
        }
    }
    sections
}

/// Extracts concise acceptance criteria lines from an issue or §7 description.
pub fn extract_acceptance_criteria(body: &str, max_items: usize) -> Vec<String> {
    if max_items == 0 {
        return Vec::new();
    }
    let mut checkboxes = Vec::new();
    let mut section_bullets = Vec::new();
    let mut in_criteria_section = false;
    for line in visible_lines(body) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((_, heading)) = headings(trimmed).next() {
            in_criteria_section = is_criteria_heading(heading);
            continue;
        }
        if let Some(lead_text) = bold_criteria_lead(trimmed) {
            in_criteria_section = true;
            if !lead_text.is_empty() && section_bullets.len() < max_items {
                section_bullets.push(lead_text);
            }
            continue;
        }
        if let Some(item) = checkbox_text(trimmed) {
            if checkboxes.len() < max_items {
                checkboxes.push(item.to_owned());
            }
            continue;
        }
        if in_criteria_section
            && let Some(item) = bullet_text(trimmed)
            && section_bullets.len() < max_items
        {
            section_bullets.push(item.to_owned());
        }
    }
    if !checkboxes.is_empty() {
        return checkboxes;
    }
    if !section_bullets.is_empty() {
        return section_bullets;
    }
    visible_lines(body)
        .filter_map(|line| bullet_text(line.trim()).map(str::to_owned))
        .take(max_items)
        .collect()
}

fn bold_criteria_lead(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with("**") {
        return None;
    }
    let after_open = trimmed.strip_prefix("**")?;
    let (lead, after_close) = after_open.split_once("**")?;
    if !is_criteria_heading(lead) {
        return None;
    }
    Some(
        after_close
            .trim()
            .trim_start_matches([':', '.', ' '])
            .to_owned(),
    )
}

fn checkbox_text(trimmed: &str) -> Option<&str> {
    trimmed
        .strip_prefix("- [ ] ")
        .or_else(|| trimmed.strip_prefix("- [x] "))
        .or_else(|| trimmed.strip_prefix("- [X] "))
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

fn bullet_text(trimmed: &str) -> Option<&str> {
    trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

/// Flips the single matching unchecked `- [ ] ` entry under `Implementation order` /
/// `Implementation plan` to `- [x] `, preserving exact line endings and all other lines.
/// Returns `Some((updated_markdown, matched_item_title))` when an unchecked item was ticked.
pub fn tick_implementation_item(markdown: &str, query: &TickQuery<'_>) -> Option<(String, String)> {
    if query.is_empty() {
        return None;
    }
    let lines = split_preserving_endings(markdown);
    let (start, end) = implementation_range(&lines)?;
    let target_index = find_matching_unchecked(&lines, start, end, query)?;
    let (line_text, ending) = lines[target_index];
    let marker = line_text.find("- [ ] ")?;
    let item_title = line_text[marker + 6..].trim().to_owned();
    let mut updated = String::with_capacity(markdown.len());
    for (index, (text, line_ending)) in lines.iter().enumerate() {
        if index == target_index {
            updated.push_str(&text[..marker]);
            updated.push_str("- [x] ");
            updated.push_str(&text[marker + 6..]);
            updated.push_str(ending);
        } else {
            updated.push_str(text);
            updated.push_str(line_ending);
        }
    }
    Some((updated, item_title))
}

fn is_criteria_heading(heading: &str) -> bool {
    let lower = heading.to_ascii_lowercase();
    lower.contains("accept")
        || lower.contains("criteria")
        || lower.starts_with("change")
        || lower.contains("related automation")
        || lower.contains("requirements")
}

fn normalize_section_id(identifier: &str) -> Option<String> {
    let trimmed = identifier.trim();
    if trimmed.is_empty() {
        return None;
    }
    section_id(trimmed)
}

fn locate_in_file(path: &Path, markdown: &str, wanted: &str) -> Option<SectionSpan> {
    let mut found: Option<(usize, usize, String)> = None;
    let mut last_line = 0;
    for (line_number, line) in numbered_visible_lines(markdown) {
        last_line = line_number;
        if let Some((level, title)) = heading_on_line(line) {
            if let Some((start_level, start_line, start_title)) = &found {
                if level <= *start_level {
                    return Some(SectionSpan {
                        id: wanted.to_owned(),
                        title: start_title.clone(),
                        path: path.to_path_buf(),
                        start_line: *start_line,
                        end_line: line_number.saturating_sub(1).max(*start_line),
                    });
                }
            } else if section_id(title).as_deref() == Some(wanted) {
                found = Some((level, line_number, title.to_owned()));
            }
        }
    }
    let total = markdown.lines().count().max(last_line);
    found.map(|(_, start_line, title)| SectionSpan {
        id: wanted.to_owned(),
        title,
        path: path.to_path_buf(),
        start_line,
        end_line: total.max(start_line),
    })
}

fn parse_open_questions(path: &Path, markdown: &str) -> Vec<OpenQuestion> {
    let mut in_section: Option<usize> = None;
    let mut questions = Vec::new();
    for (line_number, line) in numbered_visible_lines(markdown) {
        if let Some((level, title)) = heading_on_line(line) {
            if let Some(section_level) = in_section
                && level <= section_level
            {
                in_section = None;
            }
            if section_id(title).as_deref() == Some("§8")
                || title.to_ascii_lowercase().contains("open questions")
            {
                in_section = Some(level);
            }
            continue;
        }
        if in_section.is_none() {
            continue;
        }
        if let Some(question) = parse_question_row(path, line_number, line) {
            questions.push(question);
        }
    }
    questions
}

fn parse_question_row(path: &Path, line: usize, raw: &str) -> Option<OpenQuestion> {
    let trimmed = raw.trim();
    if !trimmed.starts_with('|') {
        return None;
    }
    let cells: Vec<&str> = trimmed
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect();
    if cells.len() < 3 {
        return None;
    }
    let id = cells[0];
    if id.is_empty()
        || id == "#"
        || id
            .chars()
            .all(|character| character == '-' || character == ':')
    {
        return None;
    }
    let question = cells[1].to_owned();
    let blocks = cells[2].to_owned();
    let lower = question.to_ascii_lowercase();
    let decided = lower.starts_with("decided")
        || lower.contains("*(proposed)*")
        || lower.contains("(built:")
        || lower.contains("built:");
    Some(OpenQuestion {
        id: id.to_owned(),
        question,
        blocks,
        decided,
        path: path.to_path_buf(),
        line,
    })
}

fn question_matches(row: &OpenQuestion, sections: &[String], keywords: &[String]) -> bool {
    let combined = format!("{} {}", row.blocks, row.question);
    if !sections.is_empty() {
        let tokens: Vec<String> = combined
            .split(|character: char| {
                !character.is_alphanumeric() && character != '.' && character != '§'
            })
            .filter_map(section_id)
            .collect();
        if sections.iter().any(|section| tokens.contains(section)) {
            return true;
        }
    }
    if !keywords.is_empty() {
        let lower = combined.to_ascii_lowercase();
        return keywords.iter().any(|keyword| lower.contains(keyword));
    }
    false
}

fn parse_implementation_items(path: &Path, markdown: &str) -> Vec<ImplementationItem> {
    let mut in_section: Option<usize> = None;
    let mut group = String::new();
    let mut items = Vec::new();
    for (line_number, line) in numbered_visible_lines(markdown) {
        if let Some((level, title)) = heading_on_line(line) {
            if let Some(section_level) = in_section {
                if level <= section_level {
                    in_section = None;
                    group.clear();
                } else {
                    group = title.trim_matches(['*', '#', ' ']).to_owned();
                }
            }
            if is_implementation_heading(title) {
                in_section = Some(level);
                group.clear();
            }
            continue;
        }
        if in_section.is_none() {
            continue;
        }
        if line.starts_with("**") {
            group = line.trim_matches(['*', '#', ' ']).to_owned();
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(title) = trimmed.strip_prefix("- [ ] ").map(str::trim)
            && !title.is_empty()
        {
            items.push(ImplementationItem {
                group: group.clone(),
                title: title.to_owned(),
                checked: false,
                path: path.to_path_buf(),
                line: line_number,
            });
        } else if let Some(title) = trimmed
            .strip_prefix("- [x] ")
            .or_else(|| trimmed.strip_prefix("- [X] "))
            .map(str::trim)
            && !title.is_empty()
        {
            items.push(ImplementationItem {
                group: group.clone(),
                title: title.to_owned(),
                checked: true,
                path: path.to_path_buf(),
                line: line_number,
            });
        }
    }
    items
}

fn is_implementation_heading(title: &str) -> bool {
    title.contains("Implementation order") || title.contains("Implementation plan")
}

fn implementation_range(lines: &[(&str, &str)]) -> Option<(usize, usize)> {
    let mut fence: Option<(u8, usize)> = None;
    let mut start: Option<(usize, usize)> = None;
    for (index, (line, _)) in lines.iter().enumerate() {
        if update_fence(line, &mut fence) {
            continue;
        }
        if let Some((level, title)) = heading_on_line(line) {
            if let Some((section_start, section_level)) = start {
                if level <= section_level {
                    return Some((section_start, index));
                }
            } else if is_implementation_heading(title) {
                start = Some((index + 1, level));
            }
        }
    }
    start.map(|(section_start, _)| (section_start, lines.len()))
}

fn find_matching_unchecked(
    lines: &[(&str, &str)],
    start: usize,
    end: usize,
    query: &TickQuery<'_>,
) -> Option<usize> {
    let mut entries = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut group = String::new();
    for (index, (line, _)) in lines.iter().enumerate().take(end).skip(start) {
        if update_fence(line, &mut fence) {
            continue;
        }
        if line.starts_with("**") || line.starts_with('#') {
            group = line.trim_matches(['*', '#', ' ']).to_owned();
            continue;
        }
        if let Some(title) = line.trim_start().strip_prefix("- [ ] ").map(str::trim)
            && !title.is_empty()
        {
            entries.push((index, group.clone(), title));
        }
    }
    if let Some(section) = query.spec_section.map(str::trim).filter(|s| !s.is_empty()) {
        let wanted_full = section.to_ascii_lowercase();
        let wanted_title = section
            .split_once('/')
            .map(|(_, title)| title.trim().to_ascii_lowercase());
        if let Some((index, _, _)) = entries.iter().find(|(_, group, title)| {
            format!("{group}/{title}").to_ascii_lowercase() == wanted_full
                || title.to_ascii_lowercase() == wanted_full
        }) {
            return Some(*index);
        }
        if let Some(wanted_title) = wanted_title
            && let Some((index, _, _)) = entries
                .iter()
                .find(|(_, _, title)| title.to_ascii_lowercase() == wanted_title)
        {
            return Some(*index);
        }
    }
    if let Some(issue) = query.issue_number
        && let Some((index, _, _)) = entries
            .iter()
            .find(|(_, _, title)| contains_issue_number(title, issue))
    {
        return Some(*index);
    }
    if let Some(title) = query.title.map(str::trim).filter(|s| !s.is_empty()) {
        let wanted = title.to_ascii_lowercase();
        if let Some((index, _, _)) = entries
            .iter()
            .find(|(_, _, item)| item.to_ascii_lowercase() == wanted)
        {
            return Some(*index);
        }
    }
    None
}

fn contains_issue_number(text: &str, number: u32) -> bool {
    let needle = format!("#{number}");
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(offset) = text[start..].find(&needle) {
        let position = start + offset;
        let before_ok = position == 0 || !bytes[position - 1].is_ascii_alphanumeric();
        let after = position + needle.len();
        let after_ok = after == bytes.len() || !bytes[after].is_ascii_digit();
        if before_ok && after_ok {
            return true;
        }
        start = after;
    }
    false
}

fn split_preserving_endings(markdown: &str) -> Vec<(&str, &str)> {
    let mut lines = Vec::new();
    let mut offset = 0;
    while offset < markdown.len() {
        match markdown[offset..].find('\n') {
            Some(pos) => {
                let end = offset + pos;
                if end > offset && markdown.as_bytes()[end - 1] == b'\r' {
                    lines.push((&markdown[offset..end - 1], "\r\n"));
                } else {
                    lines.push((&markdown[offset..end], "\n"));
                }
                offset = end + 1;
            }
            None => {
                lines.push((&markdown[offset..], ""));
                break;
            }
        }
    }
    lines
}

fn numbered_visible_lines(markdown: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut fence = None;
    markdown
        .lines()
        .enumerate()
        .filter_map(move |(index, line)| {
            if update_fence(line, &mut fence) {
                None
            } else {
                Some((index + 1, line))
            }
        })
}

fn update_fence(line: &str, fence: &mut Option<(u8, usize)>) -> bool {
    let trimmed = line.trim_start();
    let marker = trimmed.as_bytes().first().copied().unwrap_or_default();
    let count = trimmed.bytes().take_while(|byte| *byte == marker).count();
    if matches!(marker, b'`' | b'~') && count >= 3 {
        match *fence {
            None => *fence = Some((marker, count)),
            Some((opening, width)) if opening == marker && count >= width => *fence = None,
            _ => (),
        }
        return true;
    }
    fence.is_some()
}

fn heading_on_line(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    (level > 0 && level <= 6 && trimmed.as_bytes().get(level) == Some(&b' ')).then(|| {
        (
            level,
            trimmed[level..].trim().trim_end_matches('#').trim_end(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpecFile;

    // Risk (#539): ticking §7 on land must match the right unchecked item (by spec_section,
    // issue number with boundary check, or title), preserve CRLF/LF and leave already-ticked
    // lines untouched.
    #[test]
    fn ticks_matching_implementation_item_without_false_issue_prefix_matches()
    -> Result<(), Box<dyn std::error::Error>> {
        let spec = "## 7. Implementation order\r\n\r\n**M6 · Work**\r\n- [ ] Earlier item (#53)\r\n- [ ] Task brief at dispatch (#539)\r\n- [x] Already done (#500)\r\n";
        let (updated, title) = tick_implementation_item(
            spec,
            &TickQuery {
                spec_section: None,
                issue_number: Some(539),
                title: None,
            },
        )
        .ok_or("issue 539 should tick")?;
        assert_eq!(title, "Task brief at dispatch (#539)");
        assert!(updated.contains("- [ ] Earlier item (#53)\r\n"));
        assert!(updated.contains("- [x] Task brief at dispatch (#539)\r\n"));
        assert!(
            tick_implementation_item(
                &updated,
                &TickQuery {
                    spec_section: None,
                    issue_number: Some(539),
                    title: None,
                },
            )
            .is_none(),
            "ticking an already-checked item must be a no-op"
        );
        Ok(())
    }

    // Risk (#539): §8 lookup must match F1 without falsely matching F14, and locate exact file:lines.
    #[test]
    fn locates_sections_and_matches_open_questions_by_token()
    -> Result<(), Box<dyn std::error::Error>> {
        let spec = ProjectSpec {
            files: vec![
                SpecFile {
                    path: PathBuf::from("spec/F9-issues.md"),
                    markdown: "### F9 · Issues\nLine two\nLine three\n".into(),
                },
                SpecFile {
                    path: PathBuf::from("spec/8-open-questions.md"),
                    markdown: "## 8. Open questions\n| # | Question | Blocks |\n|---|---|---|\n| Q1 | Decided: use Work panel (F8, F9). | F9 |\n| Q14 | Unrelated provider rule. | F14 |\n| Q19 | Another F19 item. | F19 |\n".into(),
                },
            ],
        };
        let span = spec.locate_section("F9").ok_or("missing F9 span")?;
        assert_eq!(span.path, PathBuf::from("spec/F9-issues.md"));
        assert_eq!((span.start_line, span.end_line), (1, 3));

        let matches = spec.matching_questions(&["F9".into()], &[]);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "Q1");
        assert!(matches[0].decided);
        Ok(())
    }
}
