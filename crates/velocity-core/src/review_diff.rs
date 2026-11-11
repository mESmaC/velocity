//! What a PR's unified diff changes, for PR duty's review tiers (SPEC.md F7 Tiered review):
//! which files it touches and whether every change is docs, comments or formatting.
//!
//! The reading is deliberately cautious. Anything it cannot recognise (an unknown file type,
//! a binary, a rename or mode change, a comment it cannot delimit) counts as a real change, so
//! a doubtful PR gets the full review rather than the trivial one.

/// A diff reduced to what tiering needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffSummary {
    /// Every path the diff touches, old and new paths of renames included, with `/`.
    pub paths: Vec<String>,
    /// True when every file change is docs, comments or formatting.
    pub cosmetic: bool,
}

impl DiffSummary {
    /// A diff known only by its paths, as when the diff itself is too large to read. It is
    /// never cosmetic, since nothing shows its lines are docs, comments or formatting.
    pub fn from_paths(paths: Vec<String>) -> Self {
        Self {
            paths,
            cosmetic: false,
        }
    }
}

/// One file's section of a diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FileChange {
    paths: Vec<String>,
    hunks: Vec<HunkChange>,
    /// A change the line lists cannot show: binary content, a rename or a mode change.
    opaque: bool,
}

/// Context belongs to both sides: dropping it hides reordered statements. Each hunk is
/// compared separately so a removal cannot cancel an addition elsewhere in the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct HunkChange {
    removed: Vec<String>,
    added: Vec<String>,
}

/// Reads `git diff` / `gh pr diff` output.
pub fn summarize_diff(diff: &str) -> DiffSummary {
    let files = parse_files(diff);
    let mut paths: Vec<String> = files
        .iter()
        .flat_map(|file| file.paths.iter().cloned())
        .collect();
    paths.sort();
    paths.dedup();
    DiffSummary {
        cosmetic: files.iter().all(is_cosmetic),
        paths,
    }
}

fn parse_files(diff: &str) -> Vec<FileChange> {
    let mut files: Vec<FileChange> = Vec::new();
    let mut hunk = (0usize, 0usize);
    for line in diff.lines() {
        if hunk != (0, 0) {
            hunk = hunk_line(files.last_mut(), line, hunk);
            continue;
        }
        if let Some(header) = line.strip_prefix("diff --git ") {
            files.push(FileChange::default());
            if let Some(path) = header_path(header) {
                push_path(files.last_mut(), path);
            }
        } else if let Some(range) = line.strip_prefix("@@ ") {
            hunk = hunk_size(range);
            if let Some(file) = files.last_mut() {
                file.hunks.push(HunkChange::default());
            }
        } else {
            header_line(files.last_mut(), line);
        }
    }
    files
}

/// Consumes one line of a hunk; returns the old and new line counts still to come.
fn hunk_line(
    file: Option<&mut FileChange>,
    line: &str,
    (old, new): (usize, usize),
) -> (usize, usize) {
    let Some(hunk) = file.and_then(|file| file.hunks.last_mut()) else {
        return (0, 0);
    };
    match line.as_bytes().first() {
        Some(b'-') => {
            hunk.removed.push(line[1..].to_owned());
            (old.saturating_sub(1), new)
        }
        Some(b'+') => {
            hunk.added.push(line[1..].to_owned());
            (old, new.saturating_sub(1))
        }
        // "\ No newline at end of file" belongs to the line before it.
        Some(b'\\') => (old, new),
        Some(b' ') => {
            hunk.removed.push(line[1..].to_owned());
            hunk.added.push(line[1..].to_owned());
            (old.saturating_sub(1), new.saturating_sub(1))
        }
        _ => (old.saturating_sub(1), new.saturating_sub(1)),
    }
}

/// `-a,b +c,d @@`: the hunk's old and new line counts (a missing count means 1).
fn hunk_size(range: &str) -> (usize, usize) {
    let mut counts = range.split_whitespace().take(2).map(|side| {
        let side = side.trim_start_matches(['-', '+']);
        match side.split_once(',') {
            Some((_, count)) => count.parse().unwrap_or(0),
            None => 1,
        }
    });
    (counts.next().unwrap_or(0), counts.next().unwrap_or(0))
}

fn header_line(file: Option<&mut FileChange>, line: &str) {
    let Some(file) = file else {
        return;
    };
    let path = line
        .strip_prefix("--- ")
        .or_else(|| line.strip_prefix("+++ "))
        .and_then(diff_path)
        .or_else(|| line.strip_prefix("rename from ").map(unquote))
        .or_else(|| line.strip_prefix("rename to ").map(unquote));
    if let Some(path) = path {
        push_path(Some(file), path);
    }
    let opaque = [
        "rename ",
        "old mode ",
        "new mode ",
        "Binary files ",
        "GIT binary patch",
    ];
    if opaque.iter().any(|prefix| line.starts_with(prefix)) {
        file.opaque = true;
    }
}

fn push_path(file: Option<&mut FileChange>, path: String) {
    if let Some(file) = file
        && !path.is_empty()
        && !file.paths.contains(&path)
    {
        file.paths.push(path);
    }
}

/// `a/x/y.rs` or `"b/name\twith tab"` from a `---`/`+++` line; `/dev/null` names nothing.
fn diff_path(text: &str) -> Option<String> {
    // Git ends the name with a tab when it contains a space.
    let text = text.strip_suffix('\t').unwrap_or(text);
    if text == "/dev/null" {
        return None;
    }
    let path = unquote(text);
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(&path)
            .to_owned(),
    )
}

/// The path in `diff --git a/P b/P`, when both sides name the same file. Renames and quoted
/// names are read from the lines that follow instead.
fn header_path(header: &str) -> Option<String> {
    let inner = header.strip_prefix("a/")?;
    let length = inner.len().checked_sub(3)? / 2;
    let (old, new) = (inner.get(..length)?, inner.get(length..)?);
    (new.strip_prefix(" b/")? == old).then(|| old.to_owned())
}

/// Undoes git's C-style quoting of unusual path names (`core.quotePath`).
fn unquote(text: &str) -> String {
    let Some(inner) = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
    else {
        return text.replace('\\', "/");
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut characters = inner.bytes().peekable();
    while let Some(byte) = characters.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        match characters.next() {
            Some(b't') => bytes.push(b'\t'),
            Some(b'n') => bytes.push(b'\n'),
            Some(digit @ b'0'..=b'7') => {
                let mut value = u32::from(digit - b'0');
                for _ in 0..2 {
                    if let Some(next @ b'0'..=b'7') = characters.peek().copied() {
                        value = value * 8 + u32::from(next - b'0');
                        characters.next();
                    }
                }
                bytes.push(u8::try_from(value).unwrap_or(b'?'));
            }
            Some(other) => bytes.push(other),
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// How a file type writes comments, and whether its whitespace can change freely.
struct Syntax {
    line_comments: &'static [&'static str],
    block_comments: &'static [(&'static str, &'static str)],
    /// Whitespace between tokens never changes meaning. False where indentation or line
    /// breaks are syntax (Python, YAML, shells, Lean).
    free_form: bool,
}

const C_LIKE: Syntax = Syntax {
    line_comments: &["//"],
    block_comments: &[("/*", "*/")],
    free_form: true,
};
const MARKUP: Syntax = Syntax {
    line_comments: &[],
    block_comments: &[("<!--", "-->"), ("/*", "*/")],
    free_form: true,
};
const COMPONENT: Syntax = Syntax {
    line_comments: &["//"],
    block_comments: &[("<!--", "-->"), ("/*", "*/")],
    free_form: true,
};
const HASH_FREE: Syntax = Syntax {
    line_comments: &["#"],
    block_comments: &[],
    free_form: true,
};
const HASH_LINES: Syntax = Syntax {
    line_comments: &["#"],
    block_comments: &[("<#", "#>")],
    free_form: false,
};
const LEAN: Syntax = Syntax {
    line_comments: &["--"],
    block_comments: &[("/-", "-/")],
    free_form: false,
};

fn extension(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension.to_owned(),
        // `.gitignore`, `Makefile`: the whole name stands for the type.
        _ => name.trim_start_matches('.').to_owned(),
    }
}

fn is_docs(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let stem = name.split('.').next().unwrap_or(name);
    let documents = ["md", "markdown", "mdx", "rst", "txt", "adoc"];
    documents.contains(&extension(path).as_str())
        || ["license", "licence", "notice", "authors"].contains(&stem)
        || lower.starts_with("docs/")
}

fn syntax(path: &str) -> Option<&'static Syntax> {
    Some(match extension(path).as_str() {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" | "c" | "h" | "cc"
        | "cpp" | "hpp" | "cs" | "java" | "kt" | "kts" | "go" | "swift" | "css" | "scss"
        | "less" | "json" | "jsonc" | "json5" => &C_LIKE,
        "html" | "htm" | "xml" | "svg" => &MARKUP,
        "svelte" | "vue" => &COMPONENT,
        "toml" => &HASH_FREE,
        "py" | "pyi" | "sh" | "bash" | "zsh" | "ps1" | "psm1" | "psd1" | "yaml" | "yml"
        | "gitignore" | "gitattributes" | "editorconfig" | "dockerfile" | "makefile" => &HASH_LINES,
        "lean" => &LEAN,
        _ => return None,
    })
}

fn is_cosmetic(file: &FileChange) -> bool {
    if !file.paths.is_empty() && file.paths.iter().all(|path| is_docs(path)) {
        return true;
    }
    if file.opaque {
        return false;
    }
    let mut syntaxes = file.paths.iter().map(|path| syntax(path));
    let Some(Some(syntax)) = syntaxes.next() else {
        return false;
    };
    file.hunks.iter().all(|hunk| is_cosmetic_hunk(syntax, hunk))
}

fn is_cosmetic_hunk(syntax: &Syntax, hunk: &HunkChange) -> bool {
    let code = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .filter(|line| !line.trim().is_empty() && !is_comment(syntax, line))
            .cloned()
            .collect()
    };
    let (removed, added) = (code(&hunk.removed), code(&hunk.added));
    if syntax.free_form {
        significant_text(&removed) == significant_text(&added)
    } else {
        let trimmed = |lines: &[String]| -> Vec<String> {
            lines
                .iter()
                .map(|line| line.trim_end().to_owned())
                .collect()
        };
        trimmed(&removed) == trimmed(&added)
    }
}

/// Only self-contained block comments are known to be cosmetic from a diff alone.
/// Unmatched delimiters can change whether unchanged code executes; a leading `*` can
/// also be multiplication rather than a comment continuation.
fn is_comment(syntax: &Syntax, line: &str) -> bool {
    let line = line.trim();
    if syntax
        .line_comments
        .iter()
        .any(|prefix| line.starts_with(prefix))
    {
        return true;
    }
    for (open, close) in syntax.block_comments {
        if let Some(rest) = line.strip_prefix(open) {
            return rest
                .split_once(close)
                .is_some_and(|(_, after)| after.trim().is_empty());
        }
    }
    false
}

/// The lines' text with insignificant whitespace removed: whitespace inside quotes stays,
/// and a single space stays wherever dropping it would join two words or two operator
/// characters into a different token (`a - -b`, `pub fn`).
fn significant_text(lines: &[String]) -> String {
    let mut text = String::new();
    let mut spaced = false;
    for line in lines {
        let mut quote: Option<char> = None;
        let mut escaped = false;
        for character in line.chars() {
            if let Some(open) = quote {
                text.push(character);
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == open {
                    quote = None;
                }
                continue;
            }
            if character.is_whitespace() {
                spaced = true;
                continue;
            }
            if spaced && joins_token(text.chars().last(), character) {
                text.push(' ');
            }
            spaced = false;
            if matches!(character, '"' | '\'' | '`') {
                quote = Some(character);
            }
            text.push(character);
        }
        spaced = true;
    }
    text
}

fn joins_token(previous: Option<char>, next: char) -> bool {
    let word = |character: char| character.is_alphanumeric() || character == '_';
    let operator = |character: char| "+-*/%<>=!&|^:.?#~@$".contains(character);
    previous.is_some_and(|previous| {
        (word(previous) && word(next)) || (operator(previous) && operator(next))
    })
}
