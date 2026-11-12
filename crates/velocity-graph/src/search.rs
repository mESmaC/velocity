//! `graph_find` ranking. Deterministic, after the first stage of A.TXY's COE address router:
//! words with camelCase and snake_case split, 4-character stems, rarer words weighted higher,
//! and an exact name first.

use std::collections::{HashMap, HashSet};

use crate::graph::{Graph, RecordKind};

const STOP_WORDS: [&str; 24] = [
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "where", "which", "what",
    "how", "does", "self", "none", "true", "false", "return", "pub", "fn", "let", "mut", "impl",
];

/// Words and stems per record, and how many records hold each.
pub struct Search {
    names: Vec<HashSet<String>>,
    others: Vec<HashSet<String>>,
    frequency: HashMap<String, usize>,
}

pub struct Hit {
    pub record: usize,
    pub score: f64,
    pub exact: bool,
}

impl Search {
    pub fn new(graph: &Graph) -> Self {
        let mut names = Vec::with_capacity(graph.records.len());
        let mut others = Vec::with_capacity(graph.records.len());
        let mut frequency: HashMap<String, usize> = HashMap::new();
        for record in &graph.records {
            let name = words(&format!("{} {}", record.name, record.qualified));
            let other = words(&format!(
                "{} {} {}",
                record.path, record.signature, record.doc
            ));
            for word in name.union(&other) {
                *frequency.entry(word.clone()).or_default() += 1;
            }
            names.push(name);
            others.push(other);
        }
        Self {
            names,
            others,
            frequency,
        }
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
        let total = graph.records.len().max(1) as f64;
        let lowered = query.trim().to_lowercase();
        let exact_matches = graph.named(query.trim()).len();
        let mut hits: Vec<Hit> = (0..graph.records.len())
            .filter(|index| keep(*index))
            .filter_map(|index| {
                let record = &graph.records[index];
                let weight = |word: &String| (1.0 + total / *self.frequency.get(word).unwrap_or(&1) as f64).ln();
                let mut score: f64 = wanted.intersection(&self.names[index]).map(weight).sum::<f64>() * 2.0;
                score += wanted.intersection(&self.others[index]).map(weight).sum::<f64>();
                let exact = record.name.to_lowercase() == lowered || record.qualified.to_lowercase() == lowered;
                if exact {
                    score += 20.0;
                } else if lowered.len() >= 3 && record.name.to_lowercase().contains(&lowered) {
                    score += 4.0;
                }
                // Code before prose and files: an agent looking for a name wants the symbol.
                if matches!(record.kind, RecordKind::Symbol(kind) if kind != crate::SymbolKind::Heading) {
                    score += 0.5;
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
