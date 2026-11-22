//! Rewrites same-repository issue references (`#N`, `owner/name#N`, and issue URLs) when
//! Velocity syncs issues or comments between GitHub and Collapse (#628).
//!
//! Pure and synchronous: leaves code blocks, inline code spans, HTML comments, sync footers,
//! other-repository references, pull requests, comment-fragment links, already-annotated
//! references, and not-yet-synced numbers intact.

use std::collections::{BTreeMap, BTreeSet};

/// Direction of an issue or comment sync whose same-repository references are rewritten (#628).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncReferenceDirection {
    /// Syncing a GitHub issue or comment onto Collapse.
    GitHubToCollapse,
    /// Syncing a Collapse issue or comment onto a GitHub downstream repository.
    CollapseToGitHub,
}

impl SyncReferenceDirection {
    fn origin_label(self) -> &'static str {
        match self {
            Self::GitHubToCollapse => "GitHub",
            Self::CollapseToGitHub => "Collapse",
        }
    }

    fn source_hosts(self) -> [&'static str; 2] {
        match self {
            Self::GitHubToCollapse => ["github.com", "www.github.com"],
            Self::CollapseToGitHub => ["collapse.sh", "www.collapse.sh"],
        }
    }

    fn target_base_url(self) -> &'static str {
        match self {
            Self::GitHubToCollapse => "https://collapse.sh",
            Self::CollapseToGitHub => "https://github.com",
        }
    }
}

/// Output of rewriting same-repository issue references in a synced body or comment (#628).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewriteOutcome {
    /// Text with mapped same-repository references rewritten.
    pub text: String,
    /// Whether any reference in the text changed.
    pub changed: bool,
    /// Same-repository source issue numbers that had no mapping yet and were left untouched.
    pub unresolved: BTreeSet<u32>,
}

/// Inverts a `github_number -> collapse_number` map into `collapse_number -> github_number`
/// for reverse Collapse-to-GitHub reference rewriting (#628).
pub fn invert_synced_numbers(github_to_collapse: &BTreeMap<u32, u32>) -> BTreeMap<u32, u32> {
    github_to_collapse
        .iter()
        .map(|(&github, &collapse)| (collapse, github))
        .collect()
}

/// Rewrites same-repository GitHub issue references in `text` to mapped Collapse numbers,
/// keeping the original in brackets (for example `#626 (GitHub #615)`) (#628).
pub fn rewrite_github_references_for_collapse(
    text: &str,
    github_repository: &str,
    collapse_repository: &str,
    github_to_collapse: &BTreeMap<u32, u32>,
) -> RewriteOutcome {
    rewrite_synced_issue_references(
        text,
        SyncReferenceDirection::GitHubToCollapse,
        github_repository,
        collapse_repository,
        github_to_collapse,
    )
}

/// Rewrites same-repository Collapse issue references in `text` to mapped GitHub numbers,
/// keeping the original in brackets (for example `#615 (Collapse #626)`) (#628).
pub fn rewrite_collapse_references_for_github(
    text: &str,
    collapse_repository: &str,
    github_repository: &str,
    collapse_to_github: &BTreeMap<u32, u32>,
) -> RewriteOutcome {
    rewrite_synced_issue_references(
        text,
        SyncReferenceDirection::CollapseToGitHub,
        collapse_repository,
        github_repository,
        collapse_to_github,
    )
}

/// Rewrites same-repository references (`#N`, `owner/name#N`, and issue URLs) in `text`
/// according to `direction` and `number_map` (#628).
pub fn rewrite_synced_issue_references(
    text: &str,
    direction: SyncReferenceDirection,
    source_repository: &str,
    target_repository: &str,
    number_map: &BTreeMap<u32, u32>,
) -> RewriteOutcome {
    let source_repo = source_repository.trim();
    let target_repo = target_repository.trim();
    if text.is_empty() || source_repo.is_empty() || target_repo.is_empty() {
        return RewriteOutcome {
            text: text.to_owned(),
            changed: false,
            unresolved: BTreeSet::new(),
        };
    }
    let context = RewriteContext {
        direction,
        source_repo,
        target_repo,
        same_repo_name: source_repo.eq_ignore_ascii_case(target_repo),
        number_map,
    };
    let mut output = String::with_capacity(text.len());
    let mut unresolved = BTreeSet::new();
    let mut fence: Option<(char, usize)> = None;
    let mut in_html_comment = false;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if let Some((fence_char, fence_len)) = fence {
            output.push_str(line);
            if is_closing_fence(content, fence_char, fence_len) {
                fence = None;
            }
            continue;
        }
        if !in_html_comment && let Some(opened) = opening_fence(content) {
            fence = Some(opened);
            output.push_str(line);
            continue;
        }
        if !in_html_comment && is_sync_footer_line(content) {
            output.push_str(line);
            continue;
        }
        rewrite_line(
            line,
            &context,
            &mut in_html_comment,
            &mut output,
            &mut unresolved,
        );
    }
    let changed = output != text;
    RewriteOutcome {
        text: output,
        changed,
        unresolved,
    }
}

struct RewriteContext<'a> {
    direction: SyncReferenceDirection,
    source_repo: &'a str,
    target_repo: &'a str,
    same_repo_name: bool,
    number_map: &'a BTreeMap<u32, u32>,
}

impl RewriteContext<'_> {
    fn origin_annotation(&self, source_number: u32) -> String {
        let label = self.direction.origin_label();
        if self.same_repo_name {
            format!("({label} #{source_number})")
        } else {
            format!("({label} {}#{source_number})", self.source_repo)
        }
    }

    fn target_issue_url(&self, target_number: u32) -> String {
        format!(
            "{}/{}/issues/{target_number}",
            self.direction.target_base_url(),
            self.target_repo
        )
    }

    fn lookup(&self, source_number: u32, unresolved: &mut BTreeSet<u32>) -> Option<u32> {
        let Some(&target_number) = self.number_map.get(&source_number) else {
            unresolved.insert(source_number);
            return None;
        };
        if target_number == source_number && self.same_repo_name {
            return None;
        }
        Some(target_number)
    }
}

fn is_sync_footer_line(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.starts_with("_Synced from GitHub") || trimmed.starts_with("_Synced from Collapse")
}

fn rewrite_line(
    line: &str,
    context: &RewriteContext<'_>,
    in_html_comment: &mut bool,
    output: &mut String,
    unresolved: &mut BTreeSet<u32>,
) {
    let mut index = 0;
    while index < line.len() {
        if *in_html_comment {
            if let Some(close) = line[index..].find("-->") {
                let end = index + close + 3;
                output.push_str(&line[index..end]);
                index = end;
                *in_html_comment = false;
            } else {
                output.push_str(&line[index..]);
                return;
            }
            continue;
        }
        if line[index..].starts_with("<!--") {
            if let Some(close) = line[index + 4..].find("-->") {
                let end = index + 4 + close + 3;
                output.push_str(&line[index..end]);
                index = end;
            } else {
                output.push_str(&line[index..]);
                *in_html_comment = true;
                return;
            }
            continue;
        }
        let next_special = next_code_or_comment_start(line, index);
        rewrite_plain_segment(&line[index..next_special], context, output, unresolved);
        index = next_special;
        if index < line.len() && line.as_bytes()[index] == b'`' {
            index = copy_inline_code(line, index, output);
        }
    }
}

fn next_code_or_comment_start(line: &str, start: usize) -> usize {
    let slice = &line[start..];
    let tick = slice.find('`').map(|offset| start + offset);
    let comment = slice.find("<!--").map(|offset| start + offset);
    match (tick, comment) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => line.len(),
    }
}

fn copy_inline_code(line: &str, start: usize, output: &mut String) -> usize {
    let bytes = line.as_bytes();
    let ticks = bytes[start..]
        .iter()
        .take_while(|&&byte| byte == b'`')
        .count();
    let after_open = start + ticks;
    if let Some(close_start) = find_closing_backticks(bytes, after_open, ticks) {
        let end = close_start + ticks;
        output.push_str(&line[start..end]);
        end
    } else {
        output.push_str(&line[start..after_open]);
        after_open
    }
}

fn rewrite_plain_segment(
    segment: &str,
    context: &RewriteContext<'_>,
    output: &mut String,
    unresolved: &mut BTreeSet<u32>,
) {
    let mut index = 0;
    while index < segment.len() {
        let rest = &segment[index..];
        if let Some(len) = origin_annotation_len(rest) {
            output.push_str(&rest[..len]);
            index += len;
            continue;
        }
        if rest.starts_with('[')
            && let Some(consumed) = try_rewrite_markdown_link(rest, context, output, unresolved)
        {
            index += consumed;
            continue;
        }
        if rest.starts_with('<')
            && let Some(consumed) = try_rewrite_autolink(rest, context, output, unresolved)
        {
            index += consumed;
            continue;
        }
        if has_url_scheme(rest) {
            index += rewrite_bare_url(rest, context, output, unresolved);
            continue;
        }
        if !prev_is_repo_char(segment, index)
            && let Some(consumed) = try_rewrite_repo_ref(rest, context, output, unresolved)
        {
            index += consumed;
            continue;
        }
        if rest.starts_with('#')
            && !prev_is_word_char(segment, index)
            && let Some(consumed) = try_rewrite_bare_hash(rest, context, output, unresolved)
        {
            index += consumed;
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        output.push(ch);
        index += ch.len_utf8();
    }
}

fn try_rewrite_markdown_link(
    rest: &str,
    context: &RewriteContext<'_>,
    output: &mut String,
    unresolved: &mut BTreeSet<u32>,
) -> Option<usize> {
    let (end, label, destination) = parse_markdown_link(rest)?;
    if !has_url_scheme(destination) {
        return None;
    }
    if let Some(after_ann) = trailing_origin_annotation_len(&rest[end..]) {
        output.push_str(&rest[..end + after_ann]);
        return Some(end + after_ann);
    }
    let Some(source_number) = parse_source_issue_url(destination, context) else {
        output.push_str(&rest[..end]);
        return Some(end);
    };
    let Some(target_number) = context.lookup(source_number, unresolved) else {
        output.push_str(&rest[..end]);
        return Some(end);
    };
    let target_url = context.target_issue_url(target_number);
    let new_label = rewrite_link_label(label, source_number, target_number, &target_url, context);
    output.push('[');
    output.push_str(&new_label);
    output.push_str("](");
    output.push_str(&target_url);
    output.push_str(") ");
    output.push_str(&context.origin_annotation(source_number));
    Some(end)
}

fn rewrite_link_label(
    label: &str,
    source_number: u32,
    target_number: u32,
    target_url: &str,
    context: &RewriteContext<'_>,
) -> String {
    let trimmed = label.trim();
    if trimmed == format!("#{source_number}") {
        return format!("#{target_number}");
    }
    if trimmed.eq_ignore_ascii_case(&format!("{}#{source_number}", context.source_repo)) {
        return format!("{}#{target_number}", context.target_repo);
    }
    if parse_source_issue_url(trimmed, context) == Some(source_number) {
        return target_url.to_owned();
    }
    label.to_owned()
}

fn try_rewrite_autolink(
    rest: &str,
    context: &RewriteContext<'_>,
    output: &mut String,
    unresolved: &mut BTreeSet<u32>,
) -> Option<usize> {
    let close = rest[1..].find(['>', '\n', '\r', ' ', '\t'])?;
    if rest.as_bytes()[1 + close] != b'>' {
        return None;
    }
    let inner = &rest[1..1 + close];
    if !has_url_scheme(inner) {
        return None;
    }
    let end = 1 + close + 1;
    if let Some(after_ann) = trailing_origin_annotation_len(&rest[end..]) {
        output.push_str(&rest[..end + after_ann]);
        return Some(end + after_ann);
    }
    let Some(source_number) = parse_source_issue_url(inner, context) else {
        output.push_str(&rest[..end]);
        return Some(end);
    };
    let Some(target_number) = context.lookup(source_number, unresolved) else {
        output.push_str(&rest[..end]);
        return Some(end);
    };
    output.push('<');
    output.push_str(&context.target_issue_url(target_number));
    output.push_str("> ");
    output.push_str(&context.origin_annotation(source_number));
    Some(end)
}

fn rewrite_bare_url(
    rest: &str,
    context: &RewriteContext<'_>,
    output: &mut String,
    unresolved: &mut BTreeSet<u32>,
) -> usize {
    let end = scan_bare_url_end(rest);
    if let Some(after_ann) = trailing_origin_annotation_len(&rest[end..]) {
        output.push_str(&rest[..end + after_ann]);
        return end + after_ann;
    }
    let url = &rest[..end];
    let Some(source_number) = parse_source_issue_url(url, context) else {
        output.push_str(url);
        return end;
    };
    let Some(target_number) = context.lookup(source_number, unresolved) else {
        output.push_str(url);
        return end;
    };
    output.push_str(&context.target_issue_url(target_number));
    output.push(' ');
    output.push_str(&context.origin_annotation(source_number));
    end
}

fn try_rewrite_repo_ref(
    rest: &str,
    context: &RewriteContext<'_>,
    output: &mut String,
    unresolved: &mut BTreeSet<u32>,
) -> Option<usize> {
    let (repo, source_number, end) = parse_repo_issue_ref(rest)?;
    if let Some(after_ann) = trailing_origin_annotation_len(&rest[end..]) {
        output.push_str(&rest[..end + after_ann]);
        return Some(end + after_ann);
    }
    if !repo.eq_ignore_ascii_case(context.source_repo) {
        output.push_str(&rest[..end]);
        return Some(end);
    }
    let Some(target_number) = context.lookup(source_number, unresolved) else {
        output.push_str(&rest[..end]);
        return Some(end);
    };
    let rendered_repo = if context.same_repo_name {
        repo
    } else {
        context.target_repo
    };
    output.push_str(rendered_repo);
    output.push('#');
    output.push_str(&target_number.to_string());
    output.push(' ');
    output.push_str(&context.origin_annotation(source_number));
    Some(end)
}

fn try_rewrite_bare_hash(
    rest: &str,
    context: &RewriteContext<'_>,
    output: &mut String,
    unresolved: &mut BTreeSet<u32>,
) -> Option<usize> {
    let (source_number, end) = parse_hash_number(rest)?;
    if let Some(after_ann) = trailing_origin_annotation_len(&rest[end..]) {
        output.push_str(&rest[..end + after_ann]);
        return Some(end + after_ann);
    }
    let Some(target_number) = context.lookup(source_number, unresolved) else {
        output.push_str(&rest[..end]);
        return Some(end);
    };
    output.push('#');
    output.push_str(&target_number.to_string());
    output.push(' ');
    output.push_str(&context.origin_annotation(source_number));
    Some(end)
}

/// Returns the byte length of a leading `(GitHub #N)`, `(GitHub owner/name#N)`,
/// `(Collapse #N)`, or `(Collapse owner/name#N)` origin annotation.
pub(crate) fn origin_annotation_len(text: &str) -> Option<usize> {
    let after_open = text.strip_prefix('(')?;
    let after_host = strip_prefix_ignoring_case(after_open, "GitHub")
        .or_else(|| strip_prefix_ignoring_case(after_open, "Collapse"))?;
    let ws = after_host
        .bytes()
        .take_while(|&byte| byte == b' ' || byte == b'\t')
        .count();
    if ws == 0 {
        return None;
    }
    let ref_slice = &after_host[ws..];
    let ref_len = parse_repo_issue_ref(ref_slice)
        .map(|(_, _, len)| len)
        .or_else(|| parse_hash_number(ref_slice).map(|(_, len)| len))?;
    let after_ref = &ref_slice[ref_len..];
    if !after_ref.starts_with(')') {
        return None;
    }
    Some(text.len() - after_ref.len() + 1)
}

fn trailing_origin_annotation_len(after_ref: &str) -> Option<usize> {
    let ws = after_ref
        .bytes()
        .take_while(|&byte| byte == b' ' || byte == b'\t')
        .count();
    origin_annotation_len(&after_ref[ws..]).map(|len| ws + len)
}

fn parse_source_issue_url(url: &str, context: &RewriteContext<'_>) -> Option<u32> {
    let after_scheme = strip_prefix_ignoring_case(url, "https://")
        .or_else(|| strip_prefix_ignoring_case(url, "http://"))?;
    let mut after_host = None;
    for host in context.direction.source_hosts() {
        let prefix = format!("{host}/");
        if let Some(rest) = strip_prefix_ignoring_case(after_scheme, &prefix) {
            after_host = Some(rest);
            break;
        }
    }
    let path = after_host?;
    let mut parts = path.splitn(4, '/');
    let (owner, name, kind, tail) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if !kind.eq_ignore_ascii_case("issues")
        || !format!("{owner}/{name}").eq_ignore_ascii_case(context.source_repo)
    {
        return None;
    }
    let tail = tail.strip_suffix('/').unwrap_or(tail);
    if tail.is_empty() || !tail.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    tail.parse::<u32>().ok().filter(|&number| number > 0)
}

fn parse_repo_issue_ref(text: &str) -> Option<(&str, u32, usize)> {
    let prefix_len = text
        .bytes()
        .take_while(|&byte| is_repo_char(byte as char))
        .count();
    if prefix_len == 0 || text.as_bytes().get(prefix_len) != Some(&b'#') {
        return None;
    }
    let repo = &text[..prefix_len];
    let (owner, name) = repo.split_once('/')?;
    if !is_valid_repo_segment(owner) || !is_valid_repo_segment(name) || name.contains('/') {
        return None;
    }
    let (number, hash_len) = parse_hash_number(&text[prefix_len..])?;
    Some((repo, number, prefix_len + hash_len))
}

fn parse_hash_number(text: &str) -> Option<(u32, usize)> {
    let after_hash = text.strip_prefix('#')?;
    let digits = after_hash.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let ends_word = after_hash[digits..]
        .chars()
        .next()
        .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
    if !ends_word {
        return None;
    }
    let number = after_hash[..digits]
        .parse::<u32>()
        .ok()
        .filter(|&value| value > 0)?;
    Some((number, 1 + digits))
}

fn parse_markdown_link(text: &str) -> Option<(usize, &str, &str)> {
    let rest = text.strip_prefix('[')?;
    let mut depth = 1usize;
    let mut close_bracket = None;
    for (offset, ch) in rest.char_indices() {
        match ch {
            '\n' | '\r' => return None,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close_bracket = Some(1 + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let close_bracket = close_bracket?;
    if text.as_bytes().get(close_bracket + 1) != Some(&b'(') {
        return None;
    }
    let after_open = close_bracket + 2;
    let close_paren = text[after_open..]
        .find([')', '\n', '\r'])
        .filter(|&offset| text.as_bytes()[after_open + offset] == b')')
        .map(|offset| after_open + offset)?;
    let raw_target = text[after_open..close_paren].trim();
    let destination = raw_target
        .strip_prefix('<')
        .and_then(|inner| inner.split_once('>').map(|(url, _)| url))
        .unwrap_or_else(|| raw_target.split_whitespace().next().unwrap_or(""));
    Some((close_paren + 1, &text[1..close_bracket], destination))
}

fn has_url_scheme(text: &str) -> bool {
    strip_prefix_ignoring_case(text, "https://").is_some()
        || strip_prefix_ignoring_case(text, "http://").is_some()
        || strip_prefix_ignoring_case(text, "velocity://").is_some()
}

fn scan_bare_url_end(text: &str) -> usize {
    let mut end = text.len();
    for (offset, ch) in text.char_indices() {
        if ch.is_whitespace() || matches!(ch, ')' | ']' | '>' | '"' | '\'') {
            end = offset;
            break;
        }
    }
    while end > 0
        && matches!(
            text.as_bytes()[end - 1],
            b'.' | b',' | b';' | b':' | b'!' | b'?'
        )
    {
        end -= 1;
    }
    end
}

fn prev_is_repo_char(text: &str, index: usize) -> bool {
    text[..index].chars().next_back().is_some_and(is_repo_char)
}

fn prev_is_word_char(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
}

fn is_repo_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/')
}

fn is_valid_repo_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn strip_prefix_ignoring_case<'text>(text: &'text str, prefix: &str) -> Option<&'text str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

fn opening_fence(line: &str) -> Option<(char, usize)> {
    let indent = line.bytes().take_while(|&byte| byte == b' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let first = rest.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let count = rest.bytes().take_while(|&byte| byte == first as u8).count();
    if count < 3 {
        return None;
    }
    if first == '`' && rest[count..].contains('`') {
        return None;
    }
    Some((first, count))
}

fn is_closing_fence(line: &str, fence_char: char, min_len: usize) -> bool {
    let indent = line.bytes().take_while(|&byte| byte == b' ').count();
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let count = rest
        .bytes()
        .take_while(|&byte| byte == fence_char as u8)
        .count();
    count >= min_len && rest[count..].trim().is_empty()
}

fn find_closing_backticks(bytes: &[u8], start: usize, ticks: usize) -> Option<usize> {
    let mut pos = start;
    while pos < bytes.len() {
        if bytes[pos] != b'`' {
            pos += 1;
            continue;
        }
        let run = bytes[pos..]
            .iter()
            .take_while(|&&byte| byte == b'`')
            .count();
        if run == ticks {
            return Some(pos);
        }
        pos += run;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        invert_synced_numbers, rewrite_collapse_references_for_github,
        rewrite_github_references_for_collapse,
    };

    /// Risk (#628): Syncing GitHub issues onto Collapse with shifted numbers without rewriting
    /// `#N`, `owner/name#N`, and issue URLs causes text blockers to point at unrelated Collapse
    /// issues. Rewriting must keep the original in brackets, leave unmapped references alone
    /// until their issue syncs, and stay idempotent across repeated passes.
    #[test]
    fn rewrites_github_body_and_comment_references_for_collapse() {
        let mut map = BTreeMap::from([(615, 626), (100, 100)]);
        let body = "\
Blocked by #615, which makes Velocity build and run on Linux.
Also see mESmaC/velocity#615 and https://github.com/mESmaC/velocity/issues/615.
Linked [#615](https://github.com/mESmaC/velocity/issues/615) and [Linux build](https://github.com/mESmaC/velocity/issues/615).
Autolink <https://github.com/mESmaC/velocity/issues/615>.
Same number #100 stays unchanged.
Not synced yet: waits for #616 and mESmaC/velocity#617.
Other repo stays untouched: mESmaC/rosen#615 and https://github.com/mESmaC/rosen/issues/615 and [#615](https://github.com/mESmaC/rosen/issues/615).
PR and fragment stay untouched: https://github.com/mESmaC/velocity/pull/615 and https://github.com/mESmaC/velocity/issues/615#issuecomment-42.
Inline code `#615` stays untouched.
```md
Blocked by #615
```
<!-- velocity-github-sync: mESmaC/velocity#615 -->
_Synced from GitHub: https://github.com/mESmaC/velocity/issues/615_";

        let pass1 = rewrite_github_references_for_collapse(
            body,
            "mESmaC/velocity",
            "mESmaC/velocity",
            &map,
        );
        assert!(pass1.changed);
        assert_eq!(pass1.unresolved, BTreeSet::from([616, 617]));
        assert!(pass1.text.starts_with(
            "Blocked by #626 (GitHub #615), which makes Velocity build and run on Linux."
        ));
        assert!(pass1.text.contains(
            "Also see mESmaC/velocity#626 (GitHub #615) and https://collapse.sh/mESmaC/velocity/issues/626 (GitHub #615)."
        ));
        assert!(pass1.text.contains(
            "Linked [#626](https://collapse.sh/mESmaC/velocity/issues/626) (GitHub #615) and [Linux build](https://collapse.sh/mESmaC/velocity/issues/626) (GitHub #615)."
        ));
        assert!(
            pass1.text.contains(
                "Autolink <https://collapse.sh/mESmaC/velocity/issues/626> (GitHub #615)."
            )
        );
        assert!(pass1.text.contains("Same number #100 stays unchanged."));
        assert!(
            pass1
                .text
                .contains("Not synced yet: waits for #616 and mESmaC/velocity#617.")
        );
        assert!(pass1.text.contains(
            "Other repo stays untouched: mESmaC/rosen#615 and https://github.com/mESmaC/rosen/issues/615 and [#615](https://github.com/mESmaC/rosen/issues/615)."
        ));
        assert!(pass1.text.contains(
            "PR and fragment stay untouched: https://github.com/mESmaC/velocity/pull/615 and https://github.com/mESmaC/velocity/issues/615#issuecomment-42."
        ));
        assert!(pass1.text.contains("Inline code `#615` stays untouched."));
        assert!(pass1.text.contains("```md\nBlocked by #615\n```"));
        assert!(
            pass1
                .text
                .contains("<!-- velocity-github-sync: mESmaC/velocity#615 -->")
        );
        assert!(
            pass1
                .text
                .contains("_Synced from GitHub: https://github.com/mESmaC/velocity/issues/615_")
        );

        // Idempotent when run again with the same map.
        let repeat = rewrite_github_references_for_collapse(
            &pass1.text,
            "mESmaC/velocity",
            "mESmaC/velocity",
            &map,
        );
        assert!(!repeat.changed);
        assert_eq!(repeat.text, pass1.text);
        assert_eq!(repeat.unresolved, BTreeSet::from([616, 617]));

        // Second pass once #616 and #617 sync rewrites the previously left-alone references
        // without double-rewriting #626 (GitHub #615).
        map.insert(616, 627);
        map.insert(617, 628);
        let pass2 = rewrite_github_references_for_collapse(
            &pass1.text,
            "mESmaC/velocity",
            "mESmaC/velocity",
            &map,
        );
        assert!(pass2.changed);
        assert!(pass2.unresolved.is_empty());
        assert!(pass2.text.contains(
            "Not synced yet: waits for #627 (GitHub #616) and mESmaC/velocity#628 (GitHub #617)."
        ));
        assert!(!pass2.text.contains("(GitHub #615) (GitHub"));
    }

    /// Risk (#628): Reverse syncing Collapse issues to a GitHub downstream must apply the exact
    /// inverse mapping (`#615 (Collapse #626)`), including Collapse URLs.
    #[test]
    fn rewrites_collapse_references_in_reverse_for_github_downstream() {
        let github_to_collapse = BTreeMap::from([(615, 626), (616, 627)]);
        let collapse_to_github = invert_synced_numbers(&github_to_collapse);
        let text =
            "Blocked by #626 and https://collapse.sh/mESmaC/velocity/issues/627; waits for #628.";
        let rewritten = rewrite_collapse_references_for_github(
            text,
            "mESmaC/velocity",
            "mESmaC/velocity",
            &collapse_to_github,
        );
        assert!(rewritten.changed);
        assert_eq!(
            rewritten.text,
            "Blocked by #615 (Collapse #626) and https://github.com/mESmaC/velocity/issues/616 (Collapse #627); waits for #628."
        );
        assert_eq!(rewritten.unresolved, BTreeSet::from([628]));

        let repeat = rewrite_collapse_references_for_github(
            &rewritten.text,
            "mESmaC/velocity",
            "mESmaC/velocity",
            &collapse_to_github,
        );
        assert!(!repeat.changed);
        assert_eq!(repeat.text, rewritten.text);
    }
}
