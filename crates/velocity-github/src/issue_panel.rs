//! The F9 rail and spec-question projection. Refreshing never opens a browser.
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{GitHubIssue, IssueClient, PullRequestError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SpecQuestion {
    pub recommendation: String,
    pub blocked_items: u32,
}

impl SpecQuestion {
    pub fn from_body(body: &str) -> Self {
        let mut recommendation = Vec::new();
        let mut collecting = false;
        let mut blocks = false;
        let mut blocked_items = 0_u32;
        for line in body.lines() {
            let plain = line.trim().replace("**", "");
            let lower = plain.to_ascii_lowercase();
            if let Some((label, answer)) = plain.split_once(':')
                && matches!(
                    label.to_ascii_lowercase().as_str(),
                    "recommended answer" | "recommendation" | "recommended"
                )
            {
                collecting = true;
                recommendation.push(answer.trim().to_owned());
            } else if collecting && (plain.is_empty() || plain.starts_with('#')) {
                collecting = false;
            } else if collecting {
                recommendation.push(plain.clone());
            }
            if lower.starts_with("blocks:") || lower.starts_with("blocked items:") {
                blocks = true;
            } else if blocks && (plain.starts_with("- ") || plain.starts_with("* ")) {
                blocked_items = blocked_items.saturating_add(1);
            } else if blocks && !plain.is_empty() {
                blocks = false;
            }
        }
        Self {
            recommendation: recommendation.join(" ").trim().to_owned(),
            blocked_items,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct IssuePanelRow {
    pub number: u32,
    pub title: String,
    pub labels: Vec<String>,
    pub scout: Option<String>,
    pub question: Option<SpecQuestion>,
    /// The body's opening paragraph, shown when the row is expanded (#209).
    #[serde(default)]
    pub summary: String,
    pub thread_identifier: Option<u32>,
    pub thread_label: Option<String>,
}

impl IssuePanelRow {
    pub fn from_issue(issue: GitHubIssue) -> Self {
        let question = issue
            .labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case("spec-question"))
            .then(|| SpecQuestion::from_body(&issue.body));
        Self {
            number: issue.number,
            title: issue.title,
            labels: issue.labels,
            scout: issue.scout,
            question,
            summary: body_summary(&issue.body),
            thread_identifier: None,
            thread_label: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct IssuePanelSnapshot {
    pub revision: u32,
    pub project_identifier: Option<u32>,
    pub repository: Option<String>,
    pub loading: bool,
    pub rows: Vec<IssuePanelRow>,
    pub error: Option<String>,
    /// Issue numbers whose rows show their details. Every row starts collapsed so a long
    /// list stays scrollable (#209); the set is per project and survives refreshes.
    #[serde(default)]
    pub expanded: Vec<u32>,
}

impl IssuePanelSnapshot {
    pub fn select(&mut self, project: Option<u32>) {
        if self.project_identifier != project {
            self.project_identifier = project;
            self.repository = None;
            self.rows.clear();
            self.expanded.clear();
            self.error = None;
            self.loading = project.is_some();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn update(&mut self, result: Result<(String, Vec<IssuePanelRow>), String>) {
        self.loading = false;
        match result {
            Ok((repository, rows)) => {
                self.repository = Some(repository);
                self.expanded
                    .retain(|number| rows.iter().any(|row| row.number == *number));
                self.rows = rows;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// Expands a collapsed row or collapses an expanded one. Returns false for a number the
    /// panel doesn't list, so a stale click changes nothing.
    pub fn toggle_expanded(&mut self, number: u32) -> bool {
        if !self.rows.iter().any(|row| row.number == number) {
            return false;
        }
        if let Some(position) = self.expanded.iter().position(|item| *item == number) {
            self.expanded.remove(position);
        } else {
            self.expanded.push(number);
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }
}

/// Characters of an issue body an expanded row shows before it truncates.
const SUMMARY_LIMIT: usize = 280;

/// The first prose paragraph of an issue body: headings, HTML comments, images and code
/// fences are skipped, since the rail has room for a few lines of plain text only.
fn body_summary(body: &str) -> String {
    let mut words = Vec::new();
    let mut in_fence = false;
    let mut in_comment = false;
    for line in body.lines() {
        let plain = line.trim();
        if plain.starts_with("```") {
            if !words.is_empty() {
                break;
            }
            in_fence = !in_fence;
            continue;
        }
        if in_comment {
            in_comment = !plain.contains("-->");
            continue;
        }
        if plain.starts_with("<!--") {
            in_comment = !plain.contains("-->");
            continue;
        }
        let skipped =
            in_fence || plain.starts_with('#') || plain.starts_with('<') || plain.starts_with("![");
        if plain.is_empty() || skipped {
            if !words.is_empty() {
                break;
            }
            continue;
        }
        words.push(plain.replace("**", ""));
    }
    truncate(words.join(" "))
}

fn truncate(text: String) -> String {
    match text.char_indices().nth(SUMMARY_LIMIT) {
        Some((end, _)) => format!("{}…", text[..end].trim_end()),
        None => text,
    }
}

/// The open issues are read in full at least this often, even when the probe saw no change.
/// Renaming a repository label, for one, changes what the rail shows without moving any
/// issue's `updatedAt`.
pub const ISSUE_FULL_REFRESH: Duration = Duration::from_secs(10 * 60);

/// The last full read of the open issues and what the probe showed just before it, kept
/// between refreshes so a refresh where nothing changed is one small request (#228).
#[derive(Default)]
pub struct IssuePanelCache {
    probe: Option<String>,
    read_at: Option<Instant>,
    panel: Option<(String, Vec<IssuePanelRow>)>,
}

impl IssueClient {
    /// The panel, read in full only when the probe shows the open issues changed. The probe
    /// reads the open-issue count and the most recently updated open issue:
    /// - a new or reopened issue changes the count, and is the newest update;
    /// - an edited title or body, or an added or removed label, moves that issue's `updatedAt`
    ///   to the newest;
    /// - a closed, deleted or transferred issue leaves the open list and lowers the count.
    ///
    /// `updatedAt` has one-second precision, so a full read every `ISSUE_FULL_REFRESH`
    /// catches two changes in one second on either side of a probe.
    pub fn cached_panel(
        &self,
        cache: &mut IssuePanelCache,
    ) -> Result<(String, Vec<IssuePanelRow>), PullRequestError> {
        let probe = self.probe_open_issues()?;
        let fresh = cache
            .read_at
            .is_some_and(|read_at| read_at.elapsed() < ISSUE_FULL_REFRESH);
        if let Some(panel) = &cache.panel
            && fresh
            && cache.probe.as_ref() == Some(&probe)
        {
            return Ok(panel.clone());
        }
        // The probe was taken before this read, so a change in between is read again next time.
        let panel = self.panel()?;
        *cache = IssuePanelCache {
            probe: Some(probe),
            read_at: Some(Instant::now()),
            panel: Some(panel.clone()),
        };
        Ok(panel)
    }

    fn probe_open_issues(&self) -> Result<String, PullRequestError> {
        let probe: serde_json::Value = self.transport.graphql_at(
            "query($owner:String!,$name:String!) { repository(owner:$owner,name:$name) {
              issues(states:OPEN,first:1,orderBy:{field:UPDATED_AT,direction:DESC}) {
                totalCount nodes { number updatedAt } } } }",
            self.transport.variables(0),
            "/data/repository/issues",
        )?;
        Ok(probe.to_string())
    }

    pub fn panel(&self) -> Result<(String, Vec<IssuePanelRow>), PullRequestError> {
        let mut rows: Vec<_> = self
            .open_issues()?
            .into_iter()
            .map(IssuePanelRow::from_issue)
            .collect();
        rows.sort_by_key(|row| {
            (
                row.question.is_none(),
                std::cmp::Reverse(
                    row.question
                        .as_ref()
                        .map_or(0, |question| question.blocked_items),
                ),
                row.number,
            )
        });
        Ok((self.transport.repository.clone(), rows))
    }

    pub fn get(&self, number: u32) -> Result<GitHubIssue, PullRequestError> {
        self.read_issue(number)
    }

    pub fn open_in_browser(&self, number: u32) -> Result<(), PullRequestError> {
        let output = crate::command::gh(&[
            "issue",
            "view",
            &number.to_string(),
            "--repo",
            &self.transport.repository,
            "--web",
        ])
        .output()?;
        if !output.status.success() {
            return Err(PullRequestError::Command {
                code: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).trim().into(),
            });
        }
        Ok(())
    }
}
