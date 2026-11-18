use std::cmp::Reverse;
use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{Value, json};
use velocity_core::text_budget::{json_text_page, preview};

use crate::GitHubIssue;

#[derive(Deserialize)]
pub(super) struct Search {
    pub query: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: usize,
}

impl Search {
    pub fn page(&self, issues: &[GitHubIssue]) -> Value {
        let phrase = self.query.trim().to_lowercase();
        let terms: BTreeSet<_> = words(&phrase).collect();
        let mut matches: Vec<_> = issues
            .iter()
            .filter_map(|issue| {
                let score = rank(issue, &phrase, &terms);
                (score > 0).then_some((Reverse(score), Reverse(issue.number), issue))
            })
            .collect();
        matches.sort_unstable_by_key(|&(score, number, _)| (score, number));
        let mut remaining = 1_900 - 128;
        let mut rows = Vec::new();
        for (_, _, issue) in matches
            .iter()
            .skip(self.offset)
            .take(self.limit.unwrap_or(10))
        {
            let row = summary(issue);
            let size = row.to_string().len() + 1;
            if size > remaining {
                break;
            }
            remaining -= size;
            rows.push(row);
        }
        let next = self.offset.saturating_add(rows.len());
        let more = matches.len().saturating_sub(next);
        json!({"issues": rows, "more": more, "next_offset": (more > 0).then_some(next)})
    }
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
}

fn rank(issue: &GitHubIssue, phrase: &str, terms: &BTreeSet<&str>) -> usize {
    let title = issue.title.to_lowercase();
    let body = issue.body.to_lowercase();
    let title_hits = words(&title)
        .filter(|word| terms.contains(word))
        .collect::<BTreeSet<_>>();
    let body_hits = words(&body)
        .filter(|word| terms.contains(word))
        .collect::<BTreeSet<_>>();
    5 * title_hits.len()
        + body_hits.len()
        + 10 * usize::from(title.contains(phrase))
        + 2 * usize::from(body.contains(phrase))
}

fn summary(issue: &GitHubIssue) -> Value {
    let first_line = issue
        .body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with(['#', '<', '`']))
        .unwrap_or_default();
    let compact = |text: &str, budget| {
        let (text_page, next) = json_text_page(text, 0, budget).unwrap_or_default();
        if next.is_some() {
            preview(text, text_page.len())
        } else {
            text_page.to_owned()
        }
    };
    json!({
        "number": issue.number,
        "state": issue.state,
        "title": compact(&issue.title, 74),
        "summary": compact(first_line, 45),
    })
}
