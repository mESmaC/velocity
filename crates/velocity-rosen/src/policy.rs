//! The project's check policy as one base commit holds it (Rosen §2 "Project
//! configuration", R7, R8): the pre-land commands, the check set a receipt names, and the
//! paths whose changes are sensitive.
//!
//! Rosen reads `rosen.toml` from a commit's tree, never from a change or a checkout. A
//! policy read at the exact base a check runs on is the policy that check certifies, and a
//! policy change counts from the base it lands on for every change checked there. A main
//! checkout's own `AGENTS.md`, or a policy read before the merge result was staged, can be
//! older than that base.

use rosen_client::{Client, ObjectId};
use rosen_core::PathPattern;

use crate::{ProjectServer, RosenError};

/// What one base commit's `rosen.toml` says a change must pass.
#[derive(Clone, Debug)]
pub struct CheckPolicy {
    /// The commit the policy was read at.
    pub base: String,
    /// The pre-land check set's commands, in order. Empty when the project names none.
    pub commands: Vec<String>,
    /// The name a receipt for those commands carries.
    pub check_set: String,
    sensitive: Vec<PathPattern>,
}

impl CheckPolicy {
    /// Whether a change to `path` takes the sensitive review tier, as Rosen tiers it.
    pub fn is_sensitive(&self, path: &str) -> bool {
        self.sensitive.iter().any(|pattern| pattern.matches(path))
    }
}

/// The policy in force at the commit `base`, or at the current base when `None`. An error
/// when Rosen can't say: a check or a tier needs the policy, and no policy is not the same
/// as an empty one.
pub async fn policy_at(
    server: &ProjectServer,
    base: Option<&str>,
) -> Result<CheckPolicy, RosenError> {
    let at = base.map(object).transpose()?;
    let client = Client::connect(server.endpoint()).await?;
    let record = client.project_config(at).await?;
    let pre_land = record.config.checks.pre_land;
    Ok(CheckPolicy {
        base: record.commit.to_string(),
        commands: pre_land.commands,
        check_set: pre_land.name,
        sensitive: record.config.sensitive,
    })
}

pub(crate) fn object(id: &str) -> Result<ObjectId, RosenError> {
    id.parse().map_err(|_| RosenError::InvalidId(id.to_owned()))
}
