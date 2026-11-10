//! Optimistic F6 landing: every check receipt is tied to both the PR head and base.
use crate::{PullRequestClient, PullRequestError};
use serde::Serialize;
use serde_json::{Value, json};
use velocity_core::landing::{CheckScope, check_scope};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct LandingCheckpoint {
    pub head: String,
    pub base: String,
    pub scope: String,
}

impl PullRequestClient {
    pub(crate) fn base_head(&self, number: u32) -> Result<String, PullRequestError> {
        // GitHub may retain an older baseRefOid until it recalculates the PR.
        // The branch target is the live base that the merge must be checked against.
        let query = "query($owner:String!,$name:String!,$number:Int!) { repository(owner:$owner,name:$name) { pullRequest(number:$number) { baseRef { target { oid } } } } }";
        self.graphql_at(
            query,
            self.variables(number),
            "/data/repository/pullRequest/baseRef/target/oid",
        )
    }

    /// GitHub merges the current base into the head without rewriting author commits.
    pub(crate) fn prepare_landing(
        &self,
        number: u32,
        previous: Option<&LandingCheckpoint>,
    ) -> Result<LandingCheckpoint, PullRequestError> {
        let before = self.snapshot(number)?.pull_request;
        let base = self.base_head(number)?;
        let behind = self.behind(&base, &before.head_ref_oid)?;
        let overlap = match previous {
            Some(prior) if prior.base != base => self.base_overlap(&prior.base, &base, number)?,
            _ => false,
        };
        if behind {
            let endpoint = format!("repos/{}/pulls/{number}/update-branch", self.repository);
            if let Err(error) = self.command.api::<Value>(
                "PUT",
                &endpoint,
                &json!({"expected_head_sha": before.head_ref_oid}),
            ) {
                if let Some(record) = &self.review_record {
                    record.conflict(number, &before.head_ref_oid, &error.to_string())?;
                }
                return Err(error);
            }
        }
        let head = self.snapshot(number)?.pull_request.head_ref_oid;
        let clean_update = self.clean_base_update(&before.head_ref_oid, &head, &base)?;
        let scope = previous.map_or(CheckScope::Full, |prior| {
            // An automatic clean base merge contains no new author work.
            check_scope(
                prior.head == before.head_ref_oid && clean_update,
                prior.base == base,
                overlap,
            )
        });
        if self.behind(&base, &head)? {
            return Err(PullRequestError::Invalid(
                "Branch update is pending; prepare landing again before checking".into(),
            ));
        }
        self.carry_clean_approval(number, &before.head_ref_oid, &head, &base)?;
        Ok(LandingCheckpoint {
            head,
            base,
            scope: match scope {
                CheckScope::None => "none",
                CheckScope::BuildAndLint => "build_and_lint",
                CheckScope::Full => "full",
            }
            .into(),
        })
    }

    fn behind(&self, base: &str, head: &str) -> Result<bool, PullRequestError> {
        let comparison: Value = self.command.api(
            "GET",
            &format!("repos/{}/compare/{base}...{head}", self.repository),
            &json!({}),
        )?;
        Ok(comparison
            .get("behind_by")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            > 0)
    }

    fn carry_clean_approval(
        &self,
        number: u32,
        old: &str,
        head: &str,
        base: &str,
    ) -> Result<(), PullRequestError> {
        if head == old {
            return Ok(());
        }
        let Some(record) = &self.review_record else {
            return Ok(());
        };
        record.updated(number, head)?;
        let clean = self.clean_base_update(old, head, base)?;
        if clean
            && record
                .approver(number, old)
                .is_ok_and(|nick| nick == record.nick())
        {
            record.approve(number, head)?;
        }
        Ok(())
    }

    fn clean_base_update(
        &self,
        old: &str,
        head: &str,
        base: &str,
    ) -> Result<bool, PullRequestError> {
        if old == head {
            return Ok(true);
        }
        let commit: Value = self.command.api(
            "GET",
            &format!("repos/{}/commits/{head}", self.repository),
            &json!({}),
        )?;
        let parents = commit.get("parents").and_then(Value::as_array);
        let clean = parents.is_some_and(|parents| {
            parents.len() == 2
                && parents[0].get("sha").and_then(Value::as_str) == Some(old)
                && parents[1].get("sha").and_then(Value::as_str) == Some(base)
        });
        Ok(clean)
    }

    fn base_overlap(&self, old: &str, new: &str, number: u32) -> Result<bool, PullRequestError> {
        let changes: Value = self.command.api(
            "GET",
            &format!("repos/{}/compare/{old}...{new}", self.repository),
            &json!({}),
        )?;
        let Some(files) = changes.get("files").and_then(Value::as_array) else {
            return Ok(true);
        };
        // GitHub caps compare files at 300. Missing evidence never permits skipping checks.
        if files.len() >= 300 {
            return Ok(true);
        }
        let own = self.changed_files(number)?;
        Ok(files.iter().any(|file| {
            file.get("filename")
                .and_then(Value::as_str)
                .is_some_and(|path| {
                    velocity_core::landing::contested_file(path)
                        && own.iter().any(|own| own == path)
                })
        }))
    }
}
