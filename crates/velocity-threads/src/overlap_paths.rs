//! Which of the paths an issue names can hold it behind another issue's open change
//! (F9 Overlapping issues, #505). Pure, so the hold and the land check share one rule.
use std::collections::{BTreeMap, BTreeSet};

/// A path this many ready issues name still counts, however small the backlog is.
/// Without the floor, two issues naming one file in a backlog of three would never hold.
const FEW_ISSUES: usize = 3;

/// A path more than one in this many ready issues name says nothing about a conflict.
const BACKLOG_SHARE: usize = 3;

/// Whether `path` is a document or reference file. Specs, design references and other
/// documents are cited by most issues, so sharing one says nothing about a conflict;
/// Rosen's claims and the merge settle edits there. Counting them held every collapse.sh
/// issue behind one thread and turned each of its lands into false overlap reviews
/// (#505). Operations scripts under `ops/` are shared the same way (#525).
pub fn is_reference(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let in_folder = |folder: &str| path.starts_with(folder) || path.contains(&format!("/{folder}"));
    // `ops/` counts at the repository root only: a nested `ops` folder is a code module.
    [".md", ".png", ".svg", ".jpg"]
        .iter()
        .any(|extension| path.ends_with(extension))
        || path.starts_with("ops/")
        || ["spec/", "design/", "docs/"]
            .iter()
            .any(|folder| in_folder(folder))
}

/// The paths more than a few ready issues name: more than a third of the backlog and
/// more than [`FEW_ISSUES`]. A file everyone touches says nothing about who conflicts,
/// and holding on it runs the whole backlog one issue at a time (#505). `named` has one
/// entry per ready issue.
pub fn widely_shared<'named>(
    named: impl IntoIterator<Item = &'named BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut ready = 0;
    let mut issues_naming: BTreeMap<&str, usize> = BTreeMap::new();
    for paths in named {
        ready += 1;
        for path in paths {
            *issues_naming.entry(path).or_default() += 1;
        }
    }
    let limit = FEW_ISSUES.max(ready / BACKLOG_SHARE);
    issues_naming
        .into_iter()
        .filter(|&(_, issues)| issues > limit)
        .map(|(path, _)| path.to_owned())
        .collect()
}

/// Extracts explicit repository-relative file paths (such as `web/routes/explore.tsx`)
/// from issue text without inferring paths from symbol names (#649). Covers new files
/// not yet at the project base as well as paths present in the base graph.
pub fn explicit_file_paths(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !(character.is_alphanumeric() || "_./\\:-#+@".contains(character)))
        .filter_map(parse_explicit_path)
        .collect()
}

fn parse_explicit_path(mention: &str) -> Option<String> {
    let mention = mention.trim().trim_end_matches(['.', ':', '#']);
    if mention.is_empty() || mention.contains("://") {
        return None;
    }
    let normalized = mention.replace('\\', "/");
    let stripped = strip_line_anchor(normalized.trim_start_matches("./"));
    if !stripped.contains('/')
        || stripped.starts_with('/')
        || is_windows_drive_path(stripped)
        || stripped.contains('#')
        || stripped.contains(':')
        || stripped.chars().any(char::is_whitespace)
    {
        return None;
    }
    let mut leaf = "";
    for segment in stripped.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return None;
        }
        leaf = segment;
    }
    has_file_extension(leaf).then(|| stripped.to_owned())
}

fn strip_line_anchor(path: &str) -> &str {
    let path = path
        .rsplit_once('#')
        .and_then(|(prefix, fragment)| {
            let digits = fragment
                .strip_prefix('L')
                .or_else(|| fragment.strip_prefix('l'))?;
            let valid = !digits.is_empty()
                && digits.split('-').all(|part| {
                    let part = part
                        .strip_prefix('L')
                        .or_else(|| part.strip_prefix('l'))
                        .unwrap_or(part);
                    !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                });
            valid.then_some(prefix)
        })
        .unwrap_or(path);
    let mut path = path;
    while let Some((prefix, suffix)) = path.rsplit_once(':') {
        if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            break;
        }
        path = prefix;
    }
    path
}

fn is_windows_drive_path(path: &str) -> bool {
    path.as_bytes()
        .get(0..2)
        .is_some_and(|prefix| prefix[0].is_ascii_alphabetic() && prefix[1] == b':')
}

fn has_file_extension(leaf: &str) -> bool {
    let Some((stem, extension)) = leaf.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !extension.is_empty()
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && extension.bytes().any(|byte| byte.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression for #649: explicit path extraction captures concrete repo-relative paths
    // (including new files not yet at base) while ignoring capitalized words, URLs and issues.
    #[test]
    fn explicit_file_paths_extracts_concrete_paths_only() {
        let text = "Explore: add public repository cards to web/routes/explore.tsx \
            and check `src-tauri\\src\\continuous_dispatch\\overlap.rs:416` \
            (see https://collapse.sh/mESmaC/velocity/issues/649 and mESmaC/velocity#649, \
            `python stress/issue-649-unquoted-symbol-hold.py`, ratio 1/2.5).";
        let paths = explicit_file_paths(text);
        assert_eq!(
            paths,
            BTreeSet::from([
                "src-tauri/src/continuous_dispatch/overlap.rs".to_owned(),
                "stress/issue-649-unquoted-symbol-hold.py".to_owned(),
                "web/routes/explore.tsx".to_owned(),
            ])
        );
        assert_eq!(
            explicit_file_paths("Touch ui/src/routes/+page.svelte#L10-L20"),
            BTreeSet::from(["ui/src/routes/+page.svelte".to_owned()])
        );
    }
}
