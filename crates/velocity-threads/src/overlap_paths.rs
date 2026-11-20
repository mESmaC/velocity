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
