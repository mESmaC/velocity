//! A base commit and the sensitive paths in its Rosen configuration.

use rosen_client::ObjectId;
use rosen_core::PathPattern;

use crate::{ProjectServer, RosenError};

/// The review path patterns from a base commit's `rosen.toml`.
#[derive(Clone, Debug)]
pub struct CheckPolicy {
    /// The commit the policy was read at.
    pub base: String,
    sensitive: Vec<PathPattern>,
}

impl CheckPolicy {
    /// Whether a change to `path` takes the sensitive review tier, as Rosen tiers it.
    pub fn is_sensitive(&self, path: &str) -> bool {
        self.sensitive.iter().any(|pattern| pattern.matches(path))
    }
}

/// Reads the sensitive paths at the given base commit or the current base.
pub async fn policy_at(
    server: &ProjectServer,
    base: Option<&str>,
) -> Result<CheckPolicy, RosenError> {
    let at = base.map(object).transpose()?;
    let client = crate::connection::requests(server.endpoint()).await?;
    let record = client.project_config(at).await?;
    Ok(CheckPolicy {
        base: record.commit.to_string(),
        sensitive: record.config.sensitive,
    })
}

pub(crate) fn object(id: &str) -> Result<ObjectId, RosenError> {
    id.parse().map_err(|_| RosenError::InvalidId(id.to_owned()))
}
