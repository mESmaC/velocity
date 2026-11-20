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

/// The generated artifacts and registration lists in the base's `rosen.toml`. Rosen
/// regenerates the first and keeps both sides' additions in the second, so an edit to
/// either never counts as an overlap (Rosen SPEC §2).
#[derive(Clone, Debug, Default)]
pub struct MergedPaths {
    patterns: Vec<PathPattern>,
}

impl MergedPaths {
    /// Whether Rosen merges two changes' edits to `path` without a conflict.
    pub fn contains(&self, path: &str) -> bool {
        let path = path.replace('\\', "/");
        self.patterns.iter().any(|pattern| pattern.matches(&path))
    }
}

/// Reads the generated and registration lists in force at the current base. A project
/// without a base has none.
pub async fn merged_paths(server: &ProjectServer) -> Result<MergedPaths, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let config = client.base_config().await?;
    Ok(MergedPaths {
        patterns: config
            .generated
            .into_iter()
            .chain(config.registration_lists)
            .collect(),
    })
}

pub(crate) fn object(id: &str) -> Result<ObjectId, RosenError> {
    id.parse().map_err(|_| RosenError::InvalidId(id.to_owned()))
}
