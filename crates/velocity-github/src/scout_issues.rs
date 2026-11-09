use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};

use crate::issues::valid_scout_nick;
use crate::{
    GitHubIssue, IssueClient, IssueInventory, IssueReviewToken, PullRequestError, Reproduction,
};

type FilingLocks = Mutex<BTreeMap<String, Arc<Mutex<()>>>>;
static FILING_LOCKS: LazyLock<FilingLocks> = LazyLock::new(Mutex::default);

#[derive(Debug, thiserror::Error)]
pub enum ScoutIssueError {
    #[error(transparent)]
    GitHub(#[from] PullRequestError),
    #[error("Could not execute or read failure evidence: {0}")]
    Evidence(#[from] std::io::Error),
    #[error("Invalid scout request: {0}")]
    Invalid(String),
    #[error("Invalid scout JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("The reproducer did not exit with a failing status; no issue was filed")]
    NotReproduced,
    #[error("Open or closed issues changed; inspect the inventory and assess duplicates again")]
    StaleReview,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoutFinding {
    pub scout: String,
    pub title: String,
    pub description: String,
    pub labels: Vec<String>,
    pub reviewed: IssueReviewToken,
    /// Agent's semantic comparison, not a keyword heuristic. The inventory
    /// contains full bodies and closed issues; exact duplicates are also caught.
    pub duplicate_assessment: String,
    pub duplicate_of: Option<u32>,
    pub reproduction: Reproduction,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ScoutIssueOutcome {
    Duplicate {
        issue: GitHubIssue,
        reason: String,
    },
    Prepared {
        title: String,
        body: String,
        labels: Vec<String>,
    },
    Filed {
        issue: GitHubIssue,
    },
}

impl IssueClient {
    /// Runs the same validation and reproduction for preview and publication.
    /// The agent chooses `publish`; there is no user approval gate.
    pub fn file_scout_finding(
        &self,
        finding: &ScoutFinding,
        publish: bool,
    ) -> Result<ScoutIssueOutcome, ScoutIssueError> {
        finding.validate()?;
        let inventory = self.inventory()?;
        if let Some(duplicate) = finding.duplicate(&inventory, None)? {
            return Ok(duplicate);
        }
        let labels = self.resolve_labels(&finding.labels)?;
        let evidence = finding.reproduction.run(&self.directory)?;
        let body = finding.body(&evidence.attachment)?;
        let filing_lock = filing_lock(&inventory.repository)?;
        let _filing = filing_lock
            .lock()
            .map_err(|error| ScoutIssueError::Invalid(error.to_string()))?;
        // A sweep can run for minutes. Check again after evidence execution,
        // immediately before writing, rather than publishing from a stale scan.
        let current = self.inventory()?;
        if let Some(duplicate) = finding.duplicate(&current, Some(&evidence.attachment))? {
            return Ok(duplicate);
        }
        if !publish {
            return Ok(ScoutIssueOutcome::Prepared {
                title: finding.title.clone(),
                body,
                labels: finding.labels.clone(),
            });
        }
        Ok(ScoutIssueOutcome::Filed {
            issue: self.create(&finding.title, &body, &labels)?,
        })
    }

    fn resolve_labels(&self, names: &[String]) -> Result<Vec<String>, ScoutIssueError> {
        let available = self.label_identifiers()?;
        names
            .iter()
            .map(|name| {
                available.get(name).cloned().ok_or_else(|| {
                    ScoutIssueError::Invalid(format!("Unknown dispatch label: {name}"))
                })
            })
            .collect()
    }
}

fn filing_lock(repository: &str) -> Result<Arc<Mutex<()>>, ScoutIssueError> {
    // Only the final read/create pair is serialized, per repository. Sweeps and
    // reproductions still run in parallel. Unrelated gh clients can write between
    // these two API calls; GitHub has no atomic "create unless duplicate" mutation.
    let mut locks = FILING_LOCKS
        .lock()
        .map_err(|error| ScoutIssueError::Invalid(error.to_string()))?;
    Ok(Arc::clone(locks.entry(repository.to_owned()).or_default()))
}

impl ScoutFinding {
    fn validate(&self) -> Result<(), ScoutIssueError> {
        if !valid_scout_nick(&self.scout) {
            return Err(ScoutIssueError::Invalid(
                "Scout nick must be scout-N (1–16 characters)".into(),
            ));
        }
        if self.title.trim().is_empty()
            || self.title.chars().count() > 256
            || self.title.contains(['\r', '\n'])
        {
            return Err(ScoutIssueError::Invalid(
                "Title must be one nonempty line, at most 256 characters".into(),
            ));
        }
        if self.description.trim().is_empty() || self.duplicate_assessment.trim().is_empty() {
            return Err(ScoutIssueError::Invalid(
                "Describe the failure and compare it with existing open and closed issues".into(),
            ));
        }
        if self.labels.is_empty() || self.labels.iter().any(|label| label.trim().is_empty()) {
            return Err(ScoutIssueError::Invalid(
                "At least one dispatch label is required".into(),
            ));
        }
        Ok(())
    }

    fn duplicate(
        &self,
        inventory: &IssueInventory,
        attachment: Option<&str>,
    ) -> Result<Option<ScoutIssueOutcome>, ScoutIssueError> {
        if let Some(number) = self.duplicate_of {
            let issue = inventory
                .issues
                .iter()
                .find(|issue| issue.number == number)
                .ok_or_else(|| {
                    ScoutIssueError::Invalid(format!(
                        "Duplicate issue #{number} does not exist in this repository"
                    ))
                })?;
            return Ok(Some(ScoutIssueOutcome::Duplicate {
                issue: issue.clone(),
                reason: self.duplicate_assessment.clone(),
            }));
        }
        if let Some(issue) = inventory.issues.iter().find(|issue| {
            normalized_title(&issue.title) == normalized_title(&self.title)
                || attachment.is_some_and(|attachment| issue.body.contains(attachment))
        }) {
            return Ok(Some(ScoutIssueOutcome::Duplicate {
                issue: issue.clone(),
                reason: "Existing open or closed issue has the same title or failure evidence"
                    .into(),
            }));
        }
        if inventory.review_token != self.reviewed {
            return Err(ScoutIssueError::StaleReview);
        }
        Ok(None)
    }

    fn body(&self, attachment: &str) -> Result<String, ScoutIssueError> {
        let body = format!(
            "{}\n\n## Duplicate assessment\n\n{}\n\n{}\n\nFiled by {}.\n\n<!-- velocity-scout:{} -->\n",
            self.description, self.duplicate_assessment, attachment, self.scout, self.scout
        );
        if body.len() > 65_000 {
            return Err(ScoutIssueError::Invalid(
                "Issue body exceeds 65000 UTF-8 bytes; shorten the report".into(),
            ));
        }
        Ok(body)
    }
}

fn normalized_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
