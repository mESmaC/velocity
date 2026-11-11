use crate::Entry;
use std::collections::BTreeSet;

/// Retain departed aliases even when the same member later rejoins with a new nick.
pub(crate) fn mistyped_nick<'a>(nickname: &str, entries: &'a [Entry]) -> Option<&'a str> {
    if nickname.is_empty() || nickname.contains(char::is_whitespace) {
        return None;
    }
    let mut joined = BTreeSet::new();
    for record in entries {
        match record.kind.as_str() {
            "join" => {
                joined.insert(record.nick.as_str());
            }
            "leave" => {
                joined.remove(record.nick.as_str());
            }
            _ => {}
        }
    }
    let folded = nickname.to_lowercase();
    // A departed nick is still an intended addressee, regardless of edit distance.
    if let Some(record) = entries
        .iter()
        .rev()
        .find(|record| record.kind == "leave" && record.nick.to_lowercase() == folded)
    {
        return Some(&record.nick);
    }
    joined
        .into_iter()
        .map(|candidate| (edit_distance(&folded, &candidate.to_lowercase()), candidate))
        .filter(|(distance, _)| *distance <= 2)
        .min()
        .map(|(_, candidate)| candidate)
}

/// Levenshtein distance counts character insertions, deletions and substitutions.
fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<_> = right.chars().collect();
    let mut previous: Vec<_> = (0..=right.len()).collect();
    for (row, left_character) in left.chars().enumerate() {
        let mut current = vec![row + 1];
        for (column, right_character) in right.iter().enumerate() {
            current.push(
                (current[column] + 1)
                    .min(previous[column + 1] + 1)
                    .min(previous[column] + usize::from(left_character != *right_character)),
            );
        }
        previous = current;
    }
    previous[right.len()]
}
