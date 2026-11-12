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
const TEST_OR_DOC_WORDS: [&str; 9] = [
    "test", "tests", "spec", "doc", "docs", "readme", "example", "stress", "section",
];

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
        let exact_matches = graph.named(query.trim()).len();
        let tests_wanted = terms
            .iter()
            .any(|term| TEST_OR_DOC_WORDS.contains(&term.as_str()));
        let mut hits: Vec<Hit> = (0..graph.records.len())
            .filter(|index| keep(*index))
            .filter_map(|index| {
                let record = &graph.records[index];
                let exact = record.name.to_lowercase() == lowered
                    || record.qualified.to_lowercase() == lowered;
                let mut score = self.word_score(graph, index, &wanted, &terms)
                    + name_bonus(record, &lowered, exact);
                if !tests_wanted && (is_test_path(&record.path) || is_doc(record)) {
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
        let total = graph.records.len().max(1) as f64;
        let weight =
            |word: &String| (1.0 + total / *self.frequency.get(word).unwrap_or(&1) as f64).ln();
        let (name, other) = (&self.names[index], &self.others[index]);
        let score = wanted.intersection(name).map(weight).sum::<f64>() * 2.0
            + wanted.intersection(other).map(weight).sum::<f64>();
        if terms.is_empty() {
            return score;
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
        score * coverage * coverage
    }
}

fn record_words(record: &Record) -> Words {
    let name = words(&format!("{} {}", record.name, record.qualified));
    let other = words(&format!(
        "{} {} {}",
        record.path, record.signature, record.doc
    ));
    (Arc::new(name), Arc::new(other))
}

/// An exact name first, then names that start with the query, then names that contain it.
/// Code gets a nudge over prose and files: an agent looking for a name wants the symbol.
fn name_bonus(record: &Record, lowered: &str, exact: bool) -> f64 {
    let name = record.name.to_lowercase();
    let mut bonus = if exact {
        1000.0
    } else if lowered.len() >= 3 && name.starts_with(lowered) {
        30.0
    } else if lowered.len() >= 3 && name.contains(lowered) {
        4.0
    } else {
        0.0
    };
    if matches!(record.kind, RecordKind::Symbol(kind) if kind != crate::SymbolKind::Heading) {
        bonus += 0.5;
    }
    bonus
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
    matches!(record.kind, RecordKind::Symbol(crate::SymbolKind::Heading))
        || (record.kind == RecordKind::File
            && (record.path.ends_with(".md") || record.path.ends_with(".markdown")))
}

/// Lower-cased words of at least two characters, split at camelCase humps and any
/// non-alphanumeric character, plus `stem:` + the first four letters of words of five or more.
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
    out
}
