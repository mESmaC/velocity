//! Improvement scout issue search, `self-improvement` label creation, and filing (#622).
//!
//! Enforces the scout rules for Development speed findings across the tool suite
//! (`mESmaC/velocity`, `mESmaC/rosen`, `mESmaC/collapse.sh`): duplicate check against open
//! and closed issues, non-empty evidence, a validated `Target metric:` line, and automatic
//! creation of the `self-improvement` label in the destination repository when missing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use velocity_core::improvement_scout::{
    IMPROVEMENT_SCOUT_NICK, SELF_IMPROVEMENT_LABEL, parse_target_metric_line,
    validate_target_metric,
};
use velocity_core::issue_dependencies::{IssueReference, parse_blockers};
use velocity_core::issue_holds::{
    BLOCKED_UPSTREAM_LABEL, HOLD_LABEL, parse_explicit_person_hold, parse_upstream_target,
    refuse_retired_label, validate_person_hold,
};

use crate::issues::IssueBackend;
use crate::scout_search::Search;
use crate::velocity_reports::find_matching_open_issue;
use crate::{
    GitHubCommandError, IssueClient, IssueInventory, IssueReviewToken, IssueState, ScoutIssueError,
    ScoutIssueOutcome,
};

const SELF_IMPROVEMENT_LABEL_COLOR: &str = "0e8a16";
const SELF_IMPROVEMENT_LABEL_DESCRIPTION: &str =
    "Fleet self-improvement aimed at faster, cheaper development";
const MAX_BODY_BYTES: usize = 65_000;

type FilingLocks = Mutex<BTreeMap<String, Arc<Mutex<()>>>>;
static IMPROVEMENT_FILING_LOCKS: LazyLock<FilingLocks> = LazyLock::new(Mutex::default);

/// One self-improvement issue proposed by the improvement scout (#622).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImprovementIssueDraft {
    pub title: String,
    pub description: String,
    pub evidence: String,
    pub target_metric: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub reviewed: IssueReviewToken,
    pub duplicate_assessment: String,
    #[serde(default)]
    pub duplicate_of: Option<u32>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

impl IssueClient {
    /// Searches open and closed issues using the scout search ranker and returns the
    /// review token required before filing.
    pub fn search_scout_inventory(
        &self,
        query: &str,
        limit: Option<usize>,
        offset: usize,
    ) -> Result<(Value, IssueReviewToken), ScoutIssueError> {
        if !query.chars().any(char::is_alphanumeric) || limit == Some(0) {
            return Err(ScoutIssueError::Invalid(
                "Supply a query with letters or numbers and a positive limit".into(),
            ));
        }
        let inventory = self.inventory()?;
        let search = Search {
            query: query.to_owned(),
            limit,
            offset,
        };
        Ok((search.page(&inventory.issues), inventory.review_token))
    }

    /// Lists existing labels in this repository whose names match `query`.
    pub fn list_labels_matching(&self, query: &str) -> Result<Vec<String>, ScoutIssueError> {
        let query = query.to_ascii_lowercase();
        let labels = self
            .label_identifiers()?
            .into_keys()
            .filter(|label| label.to_ascii_lowercase().contains(&query))
            .collect();
        Ok(labels)
    }

    /// Creates `name` in the destination repository (GitHub or Collapse) if it does not exist.
    pub fn ensure_label(
        &self,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), GitHubCommandError> {
        if let IssueBackend::Collapse(collapse) = &self.backend {
            return collapse.ensure_label_with(name, color, description);
        }
        let transport = self.github_transport()?;
        let labels = format!("repos/{}/labels", self.repository());
        let label = format!("{labels}/{name}");
        if transport.command.read::<Value>(&["api", &label]).is_err()
            && let Err(error) = transport.command.api::<Value>(
                "POST",
                &labels,
                &json!({"name": name, "color": color, "description": description}),
            )
        {
            // Another agent may have created the label concurrently; succeed if it now exists.
            transport
                .command
                .read::<Value>(&["api", &label])
                .map_err(|_| error)?;
        }
        Ok(())
    }

    /// Validates, checks duplicates, ensures the `self-improvement` label, and previews or
    /// files one improvement scout issue (#622).
    pub fn file_improvement_issue(
        &self,
        draft: &ImprovementIssueDraft,
        publish: bool,
    ) -> Result<ScoutIssueOutcome, ScoutIssueError> {
        let (target_metric, blockers, labels) = validate_draft(draft)?;
        let inventory = self.inventory()?;
        if let Some(duplicate) = find_duplicate(draft, &inventory)? {
            return Ok(duplicate);
        }
        let resolved_blockers = self.resolve_blockers(&blockers)?;
        let blocked_by: Vec<String> = resolved_blockers
            .iter()
            .map(|blocker| blocker.name.clone())
            .collect();
        let body = format_improvement_body(draft, &target_metric)?;
        if !publish {
            return Ok(ScoutIssueOutcome::Prepared {
                title: draft.title.trim().to_owned(),
                body,
                labels,
                blocked_by,
            });
        }
        let label_ids = self.ensure_and_resolve_labels(&labels)?;
        let lock = filing_lock(&format!("{:?}:{}", self.host(), inventory.repository))?;
        let _guard = lock
            .lock()
            .map_err(|error| ScoutIssueError::Invalid(error.to_string()))?;
        let current = self.inventory()?;
        if let Some(duplicate) = find_duplicate(draft, &current)? {
            return Ok(duplicate);
        }
        let issue = self.create(draft.title.trim(), &body, &label_ids)?;
        let unlinked = self
            .add_blocked_by(&issue.id, &resolved_blockers)
            .err()
            .map(|error| format!("Its blockers were not all linked ({error})."));
        Ok(ScoutIssueOutcome::Filed {
            issue,
            blocked_by,
            unlinked,
        })
    }

    fn ensure_and_resolve_labels(&self, names: &[String]) -> Result<Vec<String>, ScoutIssueError> {
        let mut available = self.label_identifiers()?;
        if !available
            .keys()
            .any(|label| label.eq_ignore_ascii_case(SELF_IMPROVEMENT_LABEL))
        {
            self.ensure_label(
                SELF_IMPROVEMENT_LABEL,
                SELF_IMPROVEMENT_LABEL_COLOR,
                SELF_IMPROVEMENT_LABEL_DESCRIPTION,
            )?;
            available = self.label_identifiers()?;
        }
        let mut resolved = Vec::new();
        for name in names {
            let Some((_, identifier)) = available
                .iter()
                .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            else {
                return Err(ScoutIssueError::Invalid(format!(
                    "Unknown repository label: {name}"
                )));
            };
            if !resolved.contains(identifier) {
                resolved.push(identifier.clone());
            }
        }
        Ok(resolved)
    }
}

fn validate_draft(
    draft: &ImprovementIssueDraft,
) -> Result<(String, Vec<IssueReference>, Vec<String>), ScoutIssueError> {
    let title = draft.title.trim();
    if title.is_empty() || title.chars().count() > 256 || title.contains(['\r', '\n']) {
        return Err(ScoutIssueError::Invalid(
            "Title must be one nonempty line, at most 256 characters".into(),
        ));
    }
    if draft.description.trim().is_empty() {
        return Err(ScoutIssueError::Invalid(
            "Describe the proposed improvement and what step, wait or check it removes".into(),
        ));
    }
    if draft.evidence.trim().is_empty() {
        return Err(ScoutIssueError::Invalid(
            "No issue without evidence: cite concrete log, stat, friction or code evidence".into(),
        ));
    }
    if draft.duplicate_assessment.trim().is_empty() {
        return Err(ScoutIssueError::Invalid(
            "Compare this proposal against existing open and closed issues".into(),
        ));
    }
    let target_metric =
        validate_target_metric(&draft.target_metric).map_err(ScoutIssueError::Invalid)?;
    let mut labels = Vec::new();
    for raw in &draft.labels {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        validate_label(trimmed, &draft.description)?;
        if !labels
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(trimmed))
        {
            labels.push(trimmed.to_owned());
        }
    }
    if !labels
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(SELF_IMPROVEMENT_LABEL))
    {
        labels.insert(0, SELF_IMPROVEMENT_LABEL.to_owned());
    }
    let blockers = parse_blockers(&draft.blocked_by).map_err(ScoutIssueError::Invalid)?;
    Ok((target_metric, blockers, labels))
}

fn validate_label(label: &str, description: &str) -> Result<(), ScoutIssueError> {
    refuse_retired_label(label).map_err(ScoutIssueError::Invalid)?;
    if label.eq_ignore_ascii_case(BLOCKED_UPSTREAM_LABEL)
        && parse_upstream_target(description, None).is_none()
    {
        return Err(ScoutIssueError::Invalid(
            "Label `blocked-upstream` requires an upstream PR or release reference".into(),
        ));
    }
    if label.eq_ignore_ascii_case(HOLD_LABEL) {
        let Some(hold) = parse_explicit_person_hold(description) else {
            return Err(ScoutIssueError::Invalid(
                "Label `hold` requires `Hold:` and `Release when:` lines".into(),
            ));
        };
        validate_person_hold(&hold.reason, &hold.release_action)
            .map_err(ScoutIssueError::Invalid)?;
    }
    Ok(())
}

fn find_duplicate(
    draft: &ImprovementIssueDraft,
    inventory: &IssueInventory,
) -> Result<Option<ScoutIssueOutcome>, ScoutIssueError> {
    if let Some(number) = draft.duplicate_of {
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
            reason: draft.duplicate_assessment.clone(),
        }));
    }
    let target_title = normalized_title(&draft.title);
    if let Some(issue) = inventory
        .issues
        .iter()
        .find(|issue| normalized_title(&issue.title) == target_title)
    {
        return Ok(Some(ScoutIssueOutcome::Duplicate {
            issue: issue.clone(),
            reason: "Existing open or closed issue has the same title".into(),
        }));
    }
    let open: Vec<_> = inventory
        .issues
        .iter()
        .filter(|issue| issue.state == IssueState::Open)
        .cloned()
        .collect();
    if let Some(existing) =
        find_matching_open_issue(&draft.title, &draft.description, &open, |items| {
            vec![BTreeSet::new(); items.len()]
        })
    {
        return Ok(Some(ScoutIssueOutcome::Duplicate {
            issue: existing.clone(),
            reason: "Existing open issue describes the same improvement".into(),
        }));
    }
    if inventory.review_token != draft.reviewed {
        return Err(ScoutIssueError::StaleReview);
    }
    Ok(None)
}

fn format_improvement_body(
    draft: &ImprovementIssueDraft,
    target_metric: &str,
) -> Result<String, ScoutIssueError> {
    let metric_block = if parse_target_metric_line(&draft.description).is_some() {
        String::new()
    } else {
        format!("\n\nTarget metric: {target_metric}")
    };
    let body = format!(
        "{}{}\n\n## Evidence\n\n{}\n\n## Duplicate assessment\n\n{}\n\nFiled by {IMPROVEMENT_SCOUT_NICK}.\n\n<!-- velocity-scout:{IMPROVEMENT_SCOUT_NICK} -->\n",
        draft.description.trim(),
        metric_block,
        draft.evidence.trim(),
        draft.duplicate_assessment.trim(),
    );
    if body.len() > MAX_BODY_BYTES {
        return Err(ScoutIssueError::Invalid(
            "Issue body exceeds 65000 UTF-8 bytes; shorten the proposal".into(),
        ));
    }
    Ok(body)
}

fn filing_lock(repository: &str) -> Result<Arc<Mutex<()>>, ScoutIssueError> {
    let mut locks = IMPROVEMENT_FILING_LOCKS
        .lock()
        .map_err(|error| ScoutIssueError::Invalid(error.to_string()))?;
    Ok(Arc::clone(locks.entry(repository.to_owned()).or_default()))
}

fn normalized_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
