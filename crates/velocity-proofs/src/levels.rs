//! Proof levels per language (SPEC.md F17 Confidence proof): how firmly each language's code
//! is tied to a feature's proven design model.
//!
//! - **code**: a translator turned the code into Lean and `Confidence.lean` proves the
//!   translation refines the design. Rust has Aeneas; its output lives in
//!   `proofs/<Feature>/Generated/`.
//! - **reference** (the default): the model's fixture table `proofs/fixtures/<feature>.tsv`
//!   is read by code in that language, a test or a stress driver that checks the
//!   implementation against it.
//! - **model**: only the design is proven. Flagged when a stronger level is available.
//!
//! Everything here is pure; `check.rs` reads the evidence from git.

/// How firmly code is tied to the proven model, weakest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProofLevel {
    Model,
    Reference,
    Code,
}

impl ProofLevel {
    pub fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Reference => "reference",
            Self::Code => "code",
        }
    }
}

/// A language Velocity attributes code to, by file extension, and the translator that takes
/// it to the code level. Only Rust has one (Aeneas); every other language tops out at the
/// reference level until Settings → Proofs adds one (Q23). Shell and PowerShell scripts are
/// left out: they drive stress runs and are not the implementation being proven.
struct Language {
    name: &'static str,
    extensions: &'static [&'static str],
    translator: Option<&'static str>,
}

const LANGUAGES: &[Language] = &[
    Language {
        name: "Rust",
        extensions: &["rs"],
        translator: Some("Aeneas"),
    },
    Language {
        name: "TypeScript",
        extensions: &["ts", "tsx", "mts", "cts"],
        translator: None,
    },
    Language {
        name: "JavaScript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        translator: None,
    },
    Language {
        name: "Python",
        extensions: &["py"],
        translator: None,
    },
    Language {
        name: "C",
        extensions: &["c", "h"],
        translator: None,
    },
    Language {
        name: "C++",
        extensions: &["cc", "cpp", "cxx", "hh", "hpp", "hxx"],
        translator: None,
    },
    Language {
        name: "Go",
        extensions: &["go"],
        translator: None,
    },
    Language {
        name: "Java",
        extensions: &["java"],
        translator: None,
    },
    Language {
        name: "C#",
        extensions: &["cs"],
        translator: None,
    },
];

/// The language a source file is written in, by its extension (ignoring case).
pub fn language_of(path: &str) -> Option<&'static str> {
    language_entry(path).map(|language| language.name)
}

fn language_entry(path: &str) -> Option<&'static Language> {
    let file = path.rsplit(['/', '\\']).next()?;
    let (_, extension) = file.rsplit_once('.')?;
    let extension = extension.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|language| language.extensions.contains(&extension.as_str()))
}

/// The strongest level available to `language`: code where a translator exists, otherwise
/// reference, which every language reaches through a fixture table or a model driver.
pub fn strongest_available(language: &str) -> ProofLevel {
    let translated = LANGUAGES
        .iter()
        .any(|entry| entry.name == language && entry.translator.is_some());
    if translated {
        ProofLevel::Code
    } else {
        ProofLevel::Reference
    }
}

/// The fixture table name of a feature folder: `TargetedRebase` → `targeted-rebase`,
/// `ACPClient` → `acp-client`.
pub fn fixture_name(feature: &str) -> String {
    let characters: Vec<char> = feature.chars().collect();
    let mut name = String::new();
    for (position, character) in characters.iter().enumerate() {
        let previous = position.checked_sub(1).map(|index| characters[index]);
        let next = characters.get(position + 1);
        let starts_word = character.is_uppercase()
            && previous.is_some_and(|previous| {
                previous.is_lowercase()
                    || previous.is_ascii_digit()
                    || (previous.is_uppercase() && next.is_some_and(|next| next.is_lowercase()))
            });
        if starts_word {
            name.push('-');
        }
        name.extend(character.to_lowercase());
    }
    name
}

/// What ties one proof-list feature's model to code, as found in the checkout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeatureEvidence {
    /// The feature's folder under the Lake project, e.g. `TargetedRebase`.
    pub feature: String,
    /// The feature's folder from the repository root, e.g. `proofs/TargetedRebase`.
    pub folder: String,
    /// `<Feature>/Confidence.lean` exists.
    pub confidence: bool,
    /// The Aeneas translation folder, `<Feature>/Generated/`, holds Lean files.
    pub generated: bool,
    /// The feature's fixture table, when it has one.
    pub fixture: Option<String>,
    /// Tracked files outside the Lake project that name the fixture table.
    pub fixture_readers: Vec<String>,
}

/// One language's level for a feature, the strongest it could reach, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageLevel {
    /// `None` when no code in any language is tied to the model and the PR changed none.
    pub language: Option<&'static str>,
    pub level: ProofLevel,
    pub strongest: ProofLevel,
    pub evidence: String,
}

impl LanguageLevel {
    /// A model-only level where a stronger one is available: flagged on the proof card and
    /// left to the reviewer (F17 Merge gate).
    pub fn flagged(&self) -> bool {
        self.level == ProofLevel::Model && self.strongest > ProofLevel::Model
    }

    /// Below the strongest available level: what a Sensitive path refuses (F7).
    pub fn below_strongest(&self) -> bool {
        self.level < self.strongest
    }
}

/// The levels one feature reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureLevels {
    pub feature: String,
    pub languages: Vec<LanguageLevel>,
}

/// The level each language reaches for `evidence`'s feature. Languages come from the
/// evidence: the fixture table's readers (reference) and the Aeneas translation (code, Rust).
/// With no evidence at all, the PR's changed code languages (`changed_code`) are listed at
/// the model level, so a missing tie is flagged rather than silently absent.
pub fn feature_levels(evidence: &FeatureEvidence, changed_code: &[String]) -> FeatureLevels {
    let mut languages: Vec<LanguageLevel> = Vec::new();
    if evidence.generated && evidence.confidence {
        let folder = &evidence.folder;
        languages.push(level(
            Some("Rust"),
            ProofLevel::Code,
            format!(
                "Aeneas translation in `{folder}/Generated/` refined by `{folder}/Confidence.lean`"
            ),
        ));
    }
    if let Some(fixture) = &evidence.fixture {
        for language in distinct_languages(&evidence.fixture_readers) {
            if languages
                .iter()
                .any(|known| known.language == Some(language))
            {
                continue;
            }
            let readers: Vec<String> = evidence
                .fixture_readers
                .iter()
                .filter(|reader| language_of(reader) == Some(language))
                .map(|reader| format!("`{reader}`"))
                .collect();
            let evidence = format!(
                "fixture table `{fixture}` checked by {}",
                readers.join(", ")
            );
            languages.push(level(Some(language), ProofLevel::Reference, evidence));
        }
    }
    if languages.is_empty() {
        languages = untied_levels(evidence, changed_code);
    }
    languages.sort_by_key(|entry| entry.language);
    FeatureLevels {
        feature: evidence.feature.clone(),
        languages,
    }
}

fn untied_levels(evidence: &FeatureEvidence, changed_code: &[String]) -> Vec<LanguageLevel> {
    let missing = match &evidence.fixture {
        Some(fixture) => format!("no code reads the fixture table `{fixture}`"),
        None => "no fixture table and no translation tie code to the model".to_owned(),
    };
    let changed = distinct_languages(changed_code);
    if changed.is_empty() {
        return Vec::from([level(None, ProofLevel::Model, missing)]);
    }
    changed
        .into_iter()
        .map(|language| level(Some(language), ProofLevel::Model, missing.clone()))
        .collect()
}

fn level(language: Option<&'static str>, level: ProofLevel, evidence: String) -> LanguageLevel {
    LanguageLevel {
        language,
        level,
        // With no language named, any language's code could have been tied by a fixture table.
        strongest: language.map_or(ProofLevel::Reference, strongest_available),
        evidence,
    }
}

fn distinct_languages(paths: &[String]) -> Vec<&'static str> {
    let mut languages: Vec<&'static str> =
        paths.iter().filter_map(|path| language_of(path)).collect();
    languages.sort_unstable();
    languages.dedup();
    languages
}

/// The proof-list features a PR touches: those with a `<Feature>/Design.lean` in `tracked`
/// whose folder, root module (`<Feature>.lean`) or fixture table is among `changed`. Paths
/// are repository-relative with `/`; `prefix` is the Lake project folder with its slash.
pub fn touched_features(changed: &[&str], tracked: &[&str], prefix: &str) -> Vec<String> {
    let designs: Vec<&str> = tracked
        .iter()
        .filter_map(|path| path.strip_prefix(prefix)?.strip_suffix("/Design.lean"))
        .filter(|feature| !feature.contains('/'))
        .collect();
    let mut touched: Vec<String> = changed
        .iter()
        .filter_map(|path| path.strip_prefix(prefix))
        .filter_map(|rest| match rest.strip_prefix("fixtures/") {
            Some(fixture) => {
                let name = fixture.strip_suffix(".tsv")?;
                designs
                    .iter()
                    .find(|feature| fixture_name(feature) == name)
                    .copied()
            }
            None => {
                let first = rest.split('/').next()?;
                let feature = first.strip_suffix(".lean").unwrap_or(first);
                designs.contains(&feature).then_some(feature)
            }
        })
        .map(str::to_owned)
        .collect();
    touched.sort_unstable();
    touched.dedup();
    touched
}

/// The evidence for `feature` from the checkout's tracked files under the Lake project and
/// the files outside it that name the fixture table (`fixture_readers`).
pub fn feature_evidence(
    feature: &str,
    tracked: &[&str],
    prefix: &str,
    fixture_readers: Vec<String>,
) -> FeatureEvidence {
    let folder = format!("{prefix}{feature}/");
    let fixture = format!("{prefix}fixtures/{}.tsv", fixture_name(feature));
    let has_fixture = tracked.contains(&fixture.as_str());
    FeatureEvidence {
        feature: feature.to_owned(),
        folder: format!("{prefix}{feature}"),
        confidence: tracked.contains(&format!("{folder}Confidence.lean").as_str()),
        generated: tracked.iter().any(|path| {
            path.strip_prefix(&folder)
                .is_some_and(|rest| rest.starts_with("Generated/") && rest.ends_with(".lean"))
        }),
        fixture: has_fixture.then_some(fixture),
        fixture_readers: if has_fixture {
            fixture_readers
        } else {
            Vec::new()
        },
    }
}
