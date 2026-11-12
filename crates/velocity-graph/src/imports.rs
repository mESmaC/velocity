//! Imports: what a file brings into scope, so a call to an imported name resolves to the
//! definition it names (`extracted`) instead of a project-wide name match.

use serde::{Deserialize, Serialize};

/// One name a file imports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    /// The name the file uses: the last path segment, its alias, or `*` for a glob.
    pub name: String,
    /// Where it comes from: a Rust path (`crate::claims::Claim`), a JavaScript module specifier
    /// (`./ipc/updates`) or a Python module.
    pub source: String,
    pub line: u32,
    /// `export ... from` (JavaScript) or `pub use` (Rust): the name is passed on to the
    /// file's own importers.
    #[serde(default)]
    pub reexport: bool,
}

/// Expands a Rust use tree (`crate::a::{B, c::{self, D as E}, *}`) into one import per name.
pub fn rust_use_tree(tree: &str, line: u32) -> Vec<Import> {
    let mut imports = Vec::new();
    expand(String::new(), tree, line, &mut imports);
    imports
}

fn expand(prefix: String, tree: &str, line: u32, imports: &mut Vec<Import>) {
    let tree = tree.trim();
    if let Some(open) = tree.find('{') {
        let head = tree[..open].trim();
        let inner = tree[open + 1..].trim_end().trim_end_matches('}');
        let prefix = format!("{prefix}{head}");
        for item in top_level_items(inner) {
            expand(prefix.clone(), item, line, imports);
        }
        return;
    }
    let (path, alias) = match tree.split_once(" as ") {
        Some((path, alias)) => (path.trim(), Some(alias.trim())),
        None => (tree, None),
    };
    let full = format!("{prefix}{path}");
    let full = full.trim_end_matches("::self").trim_end_matches("::");
    if full.is_empty() {
        return;
    }
    let last = full.rsplit("::").next().unwrap_or(full);
    let name = alias.unwrap_or(if path == "self" {
        full.rsplit("::").next().unwrap_or(full)
    } else {
        last
    });
    if name == "_" {
        return;
    }
    imports.push(Import {
        name: name.to_owned(),
        source: full.to_owned(),
        line,
        reexport: false,
    });
}

/// Splits `a, b::{c, d}, e` on commas outside braces.
fn top_level_items(text: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    items.push(&text[start..]);
    items
        .into_iter()
        .filter(|item| !item.trim().is_empty())
        .collect()
}
