//! Idempotent issue comments and regression follow-up issue filing for #621.

use velocity_core::improvement_outcomes::RegressionFollowUpDraft;

use crate::{GitHubCommandError, GitHubIssue, IssueClient};

const SELF_IMPROVEMENT_LABEL_COLOR: &str = "0e8a16";
const SELF_IMPROVEMENT_LABEL_DESCRIPTION: &str =
    "Fleet self-improvement aimed at faster, cheaper development";

impl IssueClient {
    /// Posts the measured self-improvement outcome comment on `number` once, using `marker`
    /// so retries after ambiguous responses never double-comment (#621).
    pub fn post_improvement_outcome_comment(
        &self,
        number: u32,
        marker: &str,
        body: &str,
    ) -> Result<(), GitHubCommandError> {
        let existing = self.issue_comment_bodies(number)?;
        if existing.iter().any(|comment| comment.contains(marker)) {
            return Ok(());
        }
        let issue = self.read_issue(number)?;
        self.add_issue_comment(number, &issue.id, body)?;
        crate::issue_cache::invalidate_discussion(self.host(), self.repository(), number);
        Ok(())
    }

    /// Files one follow-up `self-improvement` issue for a regressed change, reusing any
    /// existing open or closed issue that already carries `draft.marker` (#621).
    pub fn file_improvement_regression_issue(
        &self,
        draft: &RegressionFollowUpDraft,
    ) -> Result<GitHubIssue, GitHubCommandError> {
        let inventory = self.inventory()?;
        if let Some(existing) = inventory
            .issues
            .into_iter()
            .find(|issue| issue.body.contains(&draft.marker))
        {
            return Ok(existing);
        }
        let label_id = self.ensure_single_label(&draft.label)?;
        self.create(&draft.title, &draft.body, &[label_id])
    }

    fn ensure_single_label(&self, name: &str) -> Result<String, GitHubCommandError> {
        let mut available = self.label_identifiers()?;
        if let Some((_, identifier)) = available
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
        {
            return Ok(identifier.clone());
        }
        self.ensure_label(
            name,
            SELF_IMPROVEMENT_LABEL_COLOR,
            SELF_IMPROVEMENT_LABEL_DESCRIPTION,
        )?;
        available = self.label_identifiers()?;
        available
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .map(|(_, identifier)| identifier.clone())
            .ok_or_else(|| {
                GitHubCommandError::Invalid(format!(
                    "Label `{name}` was created but could not be resolved"
                ))
            })
    }
}
