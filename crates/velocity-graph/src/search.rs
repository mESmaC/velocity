//! `graph_find` ranking. Deterministic, after the first stage of A.TXY's COE address router:
//! words with camelCase and snake_case split, 4-character stems, rarer words weighted higher,
//! and an exact name first. Coverage scaling and the test and doc weighting follow Graphify.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::graph::{Graph, Record, RecordKind};

const STOP_WORDS: [&str; 24] = [
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "where", "which", "what",
    "how", "does", "self", "none", "true", "false", "return", "pub", "fn", "let", "mut", "impl",
];

/// Query words that ask for tests or docs. Without one, those records rank lower: an agent
/// asking where something lives wants the code.
const TEST_OR_DOC_WORDS: [&str; 12] = [
    "test",
    "tests",
    "spec",
    "doc",
    "docs",
    "readme",
    "example",
    "stress",
    "section",
    "item",
    "milestone",
    "order",
];

/// Conversational words that do not carry technical signal when deciding whether an opening
/// prompt should seed code-graph matches (#534).
const CONVERSATIONAL_WORDS: [&str; 42] = [
    "about",
    "anything",
    "been",
    "chat",
    "could",
    "discuss",
    "everything",
    "feedback",
    "feel",
    "give",
    "good",
    "have",
    "hello",
    "help",
    "here",
    "hey",
    "honest",
    "just",
    "know",
    "like",
    "make",
    "more",
    "need",
    "only",
    "our",
    "please",
    "question",
    "quick",
    "really",
    "should",
    "some",
    "something",
    "sure",
    "talk",
    "tell",
    "thanks",
    "thank",
    "think",
    "thoughts",
    "want",
    "would",
    "your",
];

/// Opening phrases that mark a conversational or feedback prompt rather than a task title (#534).
const CONVERSATIONAL_PREFIXES: [&str; 17] = [
    "hi ",
    "hi,",
    "hello",
    "hey ",
    "hey,",
    "thanks",
    "thank you",
    "good morning",
    "good afternoon",
    "good evening",
    "i'd like",
    "i would like",
    "can we talk",
    "let's chat",
    "let's discuss",
    "what do you think",
    "how do you feel",
];

/// Minimum word-based relevance score (excluding the +0.5 code-symbol tie-breaker) required
/// for opening-prompt graph seeding (#534).
const TASK_SEED_MIN_SCORE: f64 = 4.0;
/// Minimum share of non-stop query terms matched for multi-term opening-prompt seeding (#534).
const TASK_SEED_MIN_COVERAGE: f64 = 0.30;

type Words = (Arc<HashSet<String>>, Arc<HashSet<String>>);

/// Words and stems per record, and how many records hold each.
pub struct Search {
    names: Vec<Arc<HashSet<String>>>,
    others: Vec<Arc<HashSet<String>>>,
    frequency: HashMap<String, usize>,
    /// Word sets by record address and content hash, so a rebuild after one edit
    /// re-tokenizes only the records that changed.
    known: HashMap<(String, String), Words>,
}

pub struct Hit {
    pub record: usize,
    pub score: f64,
    pub exact: bool,
}

impl Search {
    /// A search over nothing: a placeholder while one is rebuilt.
    pub fn empty() -> Self {
        Self {
            names: Vec::new(),
            others: Vec::new(),
            frequency: HashMap::new(),
            known: HashMap::new(),
        }
    }

    /// The search over `graph`, reusing `previous`'s word sets for unchanged records.
    pub fn new(graph: &Graph, previous: Option<Search>) -> Self {
        let mut previous = previous.map(|search| search.known).unwrap_or_default();
        let mut search = Self::empty();
        for record in &graph.records {
            let key = (record.address.clone(), record.span_hash.clone());
            let (name, other) = previous
                .remove(&key)
                .unwrap_or_else(|| record_words(record));
            for word in name.union(&other) {
                *search.frequency.entry(word.clone()).or_default() += 1;
            }
            search.known.insert(key, (name.clone(), other.clone()));
            search.names.push(name);
            search.others.push(other);
        }
        search
    }

    /// Up to `limit` records for `query`, best first, among those `keep` accepts.
    pub fn find(
        &self,
        graph: &Graph,
        query: &str,
        limit: usize,
        keep: impl Fn(usize) -> bool,
    ) -> Vec<Hit> {
        let wanted = words(query);
        let terms: Vec<&String> = wanted
            .iter()
            .filter(|word| !word.starts_with("stem:"))
            .collect();
        let lowered = query.trim().to_lowercase();
        let exact_matches = graph.named_or_aliased(query.trim()).len();
        let tests_wanted = terms
            .iter()
            .any(|term| TEST_OR_DOC_WORDS.contains(&term.as_str()))
            || crate::spec_outline::looks_like_spec_query(query);
        let mut hits: Vec<Hit> = (0..graph.records.len())
            .filter(|index| keep(*index))
            .filter_map(|index| {
                let record = &graph.records[index];
                let exact = record.name.to_lowercase() == lowered
                    || record.qualified.to_lowercase() == lowered
                    || record
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase() == lowered);
                let mut score = self.word_score(graph, index, &wanted, &terms)
                    + name_bonus(record, &lowered, exact);
                if !tests_wanted && (record.test || is_doc(record)) {
                    score *= 0.3;
                }
                (score > 0.5).then_some(Hit {
                    record: index,
                    score,
                    exact: exact && exact_matches == 1,
                })
            })
            .collect();
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then(left.record.cmp(&right.record))
        });
        hits.truncate(limit);
        hits
    }

    /// Up to `limit` records for opening-prompt task seeding, keeping only matches above the
    /// relevance threshold and returning nothing for conversational prompts (#534).
    pub fn find_task_seeds(&self, graph: &Graph, title: &str, limit: usize) -> Vec<Hit> {
        if is_conversational_prompt(title) {
            return Vec::new();
        }
        let wanted = words(title);
        let terms: Vec<&String> = wanted
            .iter()
            .filter(|word| !word.starts_with("stem:"))
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let lowered = title.trim().to_lowercase();
        let exact_matches = graph.named_or_aliased(title.trim()).len();
        let tests_wanted = terms
            .iter()
            .any(|term| TEST_OR_DOC_WORDS.contains(&term.as_str()))
            || crate::spec_outline::looks_like_spec_query(title);
        let mut hits: Vec<Hit> = (0..graph.records.len())
            .filter_map(|index| {
                let record = &graph.records[index];
                let exact = record.name.to_lowercase() == lowered
                    || record.qualified.to_lowercase() == lowered
                    || record
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase() == lowered);
                let (word_score, matched) = self.word_score_details(graph, index, &wanted, &terms);
                let name_only = name_match_bonus(record, &lowered, exact);
                let mut relevance = word_score + name_only;
                if !tests_wanted && (record.test || is_doc(record)) {
                    relevance *= 0.3;
                }
                let coverage = matched as f64 / terms.len() as f64;
                let meets_coverage = name_only > 0.0
                    || (terms.len() < 3 && matched >= 1)
                    || (matched >= 2 && coverage >= TASK_SEED_MIN_COVERAGE);
                (relevance >= TASK_SEED_MIN_SCORE && meets_coverage).then_some(Hit {
                    record: index,
                    score: relevance + code_nudge(record),
                    exact: exact && exact_matches == 1,
                })
            })
            .collect();
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then(left.record.cmp(&right.record))
        });
        hits.truncate(limit);
        hits
    }

    /// Rarer words count more and words in the name twice. The sum is scaled by the square of
    /// the share of query terms the record matches, so one common word can't outrank a record
    /// that matches the whole question (Graphify #1602).
    fn word_score(
        &self,
        graph: &Graph,
        index: usize,
        wanted: &HashSet<String>,
        terms: &[&String],
    ) -> f64 {
        self.word_score_details(graph, index, wanted, terms).0
    }

    fn word_score_details(
        &self,
        graph: &Graph,
        index: usize,
        wanted: &HashSet<String>,
        terms: &[&String],
    ) -> (f64, usize) {
        let total = graph.records.len().max(1) as f64;
        let weight =
            |word: &String| (1.0 + total / *self.frequency.get(word).unwrap_or(&1) as f64).ln();
        let (name, other) = (&self.names[index], &self.others[index]);
        let score = wanted.intersection(name).map(weight).sum::<f64>() * 2.0
            + wanted.intersection(other).map(weight).sum::<f64>();
        if terms.is_empty() {
            return (score, 0);
        }
        let matched = terms
            .iter()
            .filter(|term| {
                let stem = format!("stem:{}", term.chars().take(4).collect::<String>());
                [name, other]
                    .iter()
                    .any(|set| set.contains(**term) || set.contains(&stem))
            })
            .count();
        let coverage = matched as f64 / terms.len() as f64;
        (score * coverage * coverage, matched)
    }
}

/// Whether `text` is a conversational or feedback prompt that should not seed graph hits (#534).
pub fn is_conversational_prompt(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return true;
    }
    if has_code_or_spec_signal(trimmed) {
        return false;
    }
    let lowered = trimmed.to_ascii_lowercase();
    if CONVERSATIONAL_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix) || lowered == prefix.trim())
    {
        return true;
    }
    let meaningful: Vec<String> = words(trimmed)
        .into_iter()
        .filter(|word| !word.starts_with("stem:"))
        .collect();
    if meaningful.is_empty() {
        return true;
    }
    let technical = meaningful
        .iter()
        .filter(|word| !CONVERSATIONAL_WORDS.contains(&word.as_str()))
        .count();
    technical == 0 || (meaningful.len() >= 4 && technical * 2 < meaningful.len())
}

fn has_code_or_spec_signal(text: &str) -> bool {
    text.contains("::")
        || text.contains('`')
        || text.contains('§')
        || text.contains('/')
        || text.contains('\\')
        || text.split_whitespace().any(|token| {
            let trimmed = token.trim_matches(|character: char| {
                !character.is_alphanumeric() && !matches!(character, '_' | '.' | '#' | '-')
            });
            trimmed.contains('_')
                || trimmed.strip_prefix('#').is_some_and(|digits| {
                    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
                })
                || trimmed.strip_prefix('F').is_some_and(|digits| {
                    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
                })
                || [
                    ".rs", ".ts", ".svelte", ".md", ".toml", ".mjs", ".ps1", ".lean",
                ]
                .iter()
                .any(|suffix| trimmed.ends_with(suffix))
                || has_internal_camel_case(trimmed)
        })
}

fn has_internal_camel_case(token: &str) -> bool {
    let mut previous_lower = false;
    for character in token.chars() {
        if previous_lower && character.is_uppercase() {
            return true;
        }
        previous_lower = character.is_lowercase();
    }
    false
}

fn record_words(record: &Record) -> Words {
    let mut name = words(&format!("{} {}", record.name, record.qualified));
    for alias in &record.aliases {
        name.insert(alias.to_lowercase());
    }
    let other = words(&format!(
        "{} {} {}",
        record.path, record.signature, record.doc
    ));
    (Arc::new(name), Arc::new(other))
}

/// An exact name first, then names that start with the query, then names that contain it.
/// Code gets a nudge over prose and files: an agent looking for a name wants the symbol.
fn name_bonus(record: &Record, lowered: &str, exact: bool) -> f64 {
    name_match_bonus(record, lowered, exact) + code_nudge(record)
}

fn name_match_bonus(record: &Record, lowered: &str, exact: bool) -> f64 {
    let name = record.name.to_lowercase();
    if exact {
        1000.0
    } else if lowered.len() >= 3 && name.starts_with(lowered) {
        30.0
    } else if lowered.len() >= 3 && name.contains(lowered) {
        4.0
    } else {
        0.0
    }
}

fn code_nudge(record: &Record) -> f64 {
    if matches!(record.kind, RecordKind::Symbol(kind) if !kind.is_doc()) {
        0.5
    } else {
        0.0
    }
}

/// Test, example and stress code: ranked lower in searches that don't ask for tests, and
/// never a candidate for a production caller's call.
pub fn is_test_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.split('/').any(|part| {
        matches!(
            part,
            "tests" | "test" | "__tests__" | "examples" | "stress" | "testdata" | "fixtures"
        )
    }) || name.starts_with("test_")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
}

fn is_doc(record: &Record) -> bool {
    matches!(record.kind, RecordKind::Symbol(kind) if kind.is_doc())
        || (record.kind == RecordKind::File
            && (record.path.ends_with(".md") || record.path.ends_with(".markdown")))
}

/// Lower-cased words of at least two characters, split at camelCase humps and any
/// non-alphanumeric character, plus `stem:` + the first four letters of words of five or more,
/// and intact spec section references such as `§7`, `§3.1` and `3.1`.
pub fn words(text: &str) -> HashSet<String> {
    let mut spaced = String::with_capacity(text.len() + 8);
    let mut previous: Option<char> = None;
    for character in text.chars() {
        if character.is_uppercase()
            && previous.is_some_and(|before| before.is_lowercase() || before.is_ascii_digit())
        {
            spaced.push(' ');
        }
        spaced.push(if character.is_alphanumeric() {
            character
        } else {
            ' '
        });
        previous = Some(character);
    }
    let mut out = HashSet::new();
    for word in spaced.to_lowercase().split_whitespace() {
        if word.len() < 2 || STOP_WORDS.contains(&word) {
            continue;
        }
        if word.chars().count() >= 5 {
            out.insert(format!("stem:{}", word.chars().take(4).collect::<String>()));
        }
        out.insert(word.to_owned());
    }
    collect_section_tokens(text, &mut out);
    out
}

fn collect_section_tokens(text: &str, out: &mut HashSet<String>) {
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|character: char| {
            !character.is_alphanumeric() && !matches!(character, '§' | '.' | '/')
        });
        if token.is_empty() {
            continue;
        }
        let lower = token.to_lowercase();
        if let Some(rest) = lower.strip_prefix('§') {
            if !rest.is_empty() {
                out.insert(lower.clone());
                out.insert(rest.to_owned());
            }
        } else if lower.contains('.')
            && lower.split('.').all(|part| {
                !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
            })
        {
            out.insert(lower.clone());
            out.insert(format!("§{lower}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SymbolKind;

    fn sample_record(address: &str, name: &str, qualified: &str, path: &str, doc: &str) -> Record {
        Record {
            address: address.to_owned(),
            kind: RecordKind::Symbol(SymbolKind::Function),
            name: name.to_owned(),
            qualified: qualified.to_owned(),
            path: path.to_owned(),
            start_line: 1,
            end_line: 10,
            signature: format!("pub fn {name}()"),
            doc: doc.to_owned(),
            aliases: Vec::new(),
            span_hash: format!("hash-{address}"),
            test: false,
            up: None,
            down: Vec::new(),
        }
    }

    #[test]
    fn conversational_prompts_never_seed_graph_matches() {
        let mut graph = Graph::default();
        graph.records.push(sample_record(
            "1.aaaa1",
            "honest_feedback_helper",
            "honest_feedback_helper",
            "crates/example/src/lib.rs",
            "Handles user feedback",
        ));
        let search = Search::new(&graph, None);
        assert!(is_conversational_prompt(
            "I'd like your honest feedback on working in Velocity"
        ));
        assert!(is_conversational_prompt(
            "Hey, can we talk about how things feel?"
        ));
        assert!(
            search
                .find_task_seeds(
                    &graph,
                    "I'd like your honest feedback on working in Velocity",
                    5
                )
                .is_empty()
        );
    }

    #[test]
    fn task_seeding_keeps_relevant_matches_and_drops_weak_incidental_hits() {
        let mut graph = Graph::default();
        graph.records.push(sample_record(
            "1.aaaa1",
            "load_claude_project_instructions",
            "SessionSetup::load_claude_project_instructions",
            "crates/velocity-acp/src/client/session.rs",
            "Inject AGENTS.md once for Claude sessions",
        ));
        graph.records.push(sample_record(
            "1.aaaa2",
            "render_session_card",
            "SessionCard::render_session_card",
            "crates/velocity-acp/src/client/session_card.rs",
            "Render short per-thread session card",
        ));
        for index in 0..40 {
            graph.records.push(sample_record(
                &format!("1.bbbb{index}"),
                &format!("handle_item_{index}"),
                &format!("module::handle_item_{index}"),
                "crates/other/src/lib.rs",
                "Generic handler for misc work",
            ));
        }
        let search = Search::new(&graph, None);
        assert!(!is_conversational_prompt(
            "Agent instructions: inject once, resolve contradictions, a session card and a drift check"
        ));
        let weak = search.find_task_seeds(&graph, "generic unrelated prose without symbols", 5);
        assert!(
            weak.is_empty(),
            "expected weak incidental hit to be filtered"
        );
        let strong =
            search.find_task_seeds(&graph, "load_claude_project_instructions AGENTS.md", 5);
        assert!(!strong.is_empty());
        assert_eq!(
            graph.records[strong[0].record].name,
            "load_claude_project_instructions"
        );
    }
}
