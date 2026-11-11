//! Settings → Proofs (SPEC.md F13, F17): the project's proof list as SPEC.md records it, the
//! design proofs in its Lake project with the level each language reaches, the code-level
//! translator for each language, and the stress-diff input count scouts use. `velocity-proofs`
//! reads the checkout and builds the view; the parser here is pure.

use crate::proof_card::{ProofLevel, ProofLevelKind};

/// One feature on the project's proof list.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProofListFeature {
    pub feature: String,
    /// Why it is on the list: the rest of the bullet after `Feature:`, or the table's other
    /// columns joined with ` · `. Empty when SPEC.md gives no reason.
    pub detail: String,
}

impl ProofListFeature {
    pub fn new(feature: &str, detail: &str) -> Self {
        Self {
            feature: feature.to_owned(),
            detail: detail.to_owned(),
        }
    }
}

/// A design proof found in the Lake project, `proofs/<Feature>/Design.lean`.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesignProofEntry {
    pub feature: String,
    /// From the repository root, e.g. `proofs/TargetedRebase`.
    pub folder: String,
    /// The level each language's code reaches for this feature, read as F17 "How Velocity
    /// reads the level" says.
    pub levels: Vec<ProofLevel>,
}

impl DesignProofEntry {
    pub fn new(feature: &str, folder: &str, levels: Vec<ProofLevel>) -> Self {
        Self {
            feature: feature.to_owned(),
            folder: folder.to_owned(),
            levels,
        }
    }
}

/// A language's code-level translator, and the strongest level its code can reach.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LanguageTranslator {
    pub language: String,
    /// `None` until a translator is set for the language (Q23); it then tops out at reference.
    pub translator: Option<String>,
    pub strongest: ProofLevelKind,
}

impl LanguageTranslator {
    pub fn new(language: &str, translator: Option<&str>, strongest: ProofLevelKind) -> Self {
        Self {
            language: language.to_owned(),
            translator: translator.map(str::to_owned),
            strongest,
        }
    }
}

/// Everything the Proofs page shows for one project.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProofsSettingsView {
    /// The project has a SPEC.md at its root.
    pub spec_found: bool,
    /// SPEC.md has a proof list, even an empty one ("- None.").
    pub proof_list_found: bool,
    pub proof_list: Vec<ProofListFeature>,
    /// The Lake project's folder from the repository root, when the project has one.
    pub lake_project: Option<String>,
    pub design_proofs: Vec<DesignProofEntry>,
    pub translators: Vec<LanguageTranslator>,
    /// How many generated inputs a scout diffs between the code and the compiled Lean model
    /// for each feature (F17 reference level). Saved with the app's settings.
    pub stress_diff_input_count: u32,
}

impl ProofsSettingsView {
    /// `proof_list` is `None` when SPEC.md has no proof list at all.
    pub fn new(
        spec_found: bool,
        proof_list: Option<Vec<ProofListFeature>>,
        lake_project: Option<String>,
        design_proofs: Vec<DesignProofEntry>,
        translators: Vec<LanguageTranslator>,
        stress_diff_input_count: u32,
    ) -> Self {
        Self {
            spec_found,
            proof_list_found: proof_list.is_some(),
            proof_list: proof_list.unwrap_or_default(),
            lake_project,
            design_proofs,
            translators,
            stress_diff_input_count,
        }
    }
}

/// The proof list SPEC.md records (F17 "Who sets it"), or `None` when it has none.
///
/// Two shapes are read, the ones SPEC.md files actually use:
/// - a `Proof list` heading at any level (what the spec interview writes on lock, F3): its
///   bullets or table rows up to the next heading of the same or a higher level;
/// - a paragraph line ending in `proof list:` (as in Velocity's own F17): the bullets or table
///   right below it, up to the first blank line after them.
pub fn proof_list_from_spec(spec: &str) -> Option<Vec<ProofListFeature>> {
    let lines: Vec<&str> = spec.lines().map(str::trim_end).collect();
    let (start, heading_level) = lines.iter().enumerate().find_map(|(index, line)| {
        list_start(line).map(|heading_level| (index + 1, heading_level))
    })?;
    let mut rows: Vec<&str> = Vec::new();
    for line in &lines[start..] {
        if let Some(level) = heading_level_of(line) {
            if heading_level.is_none_or(|heading_level| level <= heading_level) {
                break;
            }
            continue;
        }
        let item = line.trim_start();
        if item.is_empty() {
            if heading_level.is_none() && !rows.is_empty() {
                break;
            }
            continue;
        }
        if item.starts_with("- ") || item.starts_with("* ") || item.starts_with('|') {
            rows.push(item);
        }
    }
    Some(features_from_rows(&rows))
}

/// `Some(heading level)` for a `Proof list` heading, `Some(None)` for a line introducing the
/// list, `None` for every other line.
fn list_start(line: &str) -> Option<Option<usize>> {
    match heading_level_of(line) {
        Some(level) => {
            let text = plain(line.trim_start_matches('#'));
            text.ends_with("proof list").then_some(Some(level))
        }
        None => plain(line).ends_with("proof list:").then_some(None),
    }
}

fn heading_level_of(line: &str) -> Option<usize> {
    let level = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    (level > 0 && line[level..].starts_with(' ')).then_some(level)
}

/// Lower case, without markdown emphasis or code marks, trimmed.
fn plain(text: &str) -> String {
    text.replace(['*', '`'], "").trim().to_lowercase()
}

fn features_from_rows(rows: &[&str]) -> Vec<ProofListFeature> {
    let mut features = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if let Some(bullet) = row.strip_prefix("- ").or_else(|| row.strip_prefix("* ")) {
            let bullet = strip_marks(bullet);
            if bullet.trim_end_matches('.').eq_ignore_ascii_case("none") {
                continue;
            }
            let (feature, detail) = bullet.split_once(": ").unwrap_or((bullet.as_str(), ""));
            features.push(ProofListFeature::new(feature.trim(), detail.trim()));
            continue;
        }
        let cells = table_cells(row);
        // A table's header is the row above its `|---|` separator; the separator says nothing.
        let header = rows.get(index + 1).is_some_and(|next| is_separator(next));
        if header || is_separator(row) {
            continue;
        }
        if let Some((feature, rest)) = cells.split_first()
            && !feature.is_empty()
        {
            features.push(ProofListFeature::new(feature, &rest.join(" · ")));
        }
    }
    features
}

fn table_cells(row: &str) -> Vec<String> {
    row.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| strip_marks(cell).trim().to_owned())
        .collect()
}

fn is_separator(row: &str) -> bool {
    row.starts_with('|')
        && row
            .chars()
            .all(|character| matches!(character, '|' | '-' | ':' | ' '))
}

fn strip_marks(text: &str) -> String {
    text.replace("**", "").replace('`', "")
}
