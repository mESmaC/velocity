//! Which parser and query read a file (F19 Languages).

use serde::{Deserialize, Serialize};

/// The languages the graph reads symbols from. Every other file is listed at plane 2 with no
/// symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    /// A Svelte component: its `<script>` blocks are read as TypeScript.
    Svelte,
    Python,
    Go,
    Java,
    CSharp,
    C,
    Cpp,
    Markdown,
}

impl Language {
    /// The language a path is read as, from its extension.
    pub fn of(path: &str) -> Option<Self> {
        let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
        Some(match extension.as_str() {
            "rs" => Self::Rust,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Self::JavaScript,
            "svelte" => Self::Svelte,
            "py" | "pyi" => Self::Python,
            "go" => Self::Go,
            "java" => Self::Java,
            "cs" => Self::CSharp,
            "c" => Self::C,
            // `.h` is shared by C and C++; the C++ grammar reads both.
            "h" | "hh" | "hpp" | "hxx" | "cc" | "cpp" | "cxx" => Self::Cpp,
            "md" | "markdown" => Self::Markdown,
            _ => return None,
        })
    }

    /// The tree-sitter grammar, or `None` for Markdown, which is read line by line.
    pub(crate) fn grammar(self) -> Option<tree_sitter::Language> {
        Some(match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript | Self::Svelte => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::Markdown => return None,
        })
    }

    /// The query that finds definitions, references and imports. Rust, TypeScript and
    /// JavaScript use Velocity's own, which also capture path-qualified calls and imports; the
    /// rest use their grammar's tags query.
    pub(crate) fn query_source(self) -> String {
        const JAVASCRIPT: &str = include_str!("queries/javascript.scm");
        match self {
            Self::Rust => include_str!("queries/rust.scm").to_owned(),
            Self::TypeScript | Self::Tsx | Self::Svelte => {
                format!("{JAVASCRIPT}\n{}", include_str!("queries/typescript.scm"))
            }
            Self::JavaScript => {
                format!(
                    "{JAVASCRIPT}\n{}",
                    include_str!("queries/javascript_only.scm")
                )
            }
            Self::Python => format!(
                "{}\n{}",
                tree_sitter_python::TAGS_QUERY,
                include_str!("queries/python.scm")
            ),
            Self::Go => tree_sitter_go::TAGS_QUERY.to_owned(),
            Self::Java => tree_sitter_java::TAGS_QUERY.to_owned(),
            Self::CSharp => tree_sitter_c_sharp::TAGS_QUERY.to_owned(),
            Self::C => tree_sitter_c::TAGS_QUERY.to_owned(),
            Self::Cpp => tree_sitter_cpp::TAGS_QUERY.to_owned(),
            Self::Markdown => String::new(),
        }
    }

    /// Whether this language's query was written for the graph: its calls carry qualifiers and
    /// its files carry imports, so a same-file or imported match can count as `extracted`.
    pub(crate) fn precise(self) -> bool {
        matches!(
            self,
            Self::Rust | Self::TypeScript | Self::Tsx | Self::JavaScript | Self::Svelte
        )
    }
}
