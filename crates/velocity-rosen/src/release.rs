//! The commit a release is cut from (#431): Rosen R12's last green commit, or a landed
//! commit it covers. R12 advances green as a prefix of the base, so every land at or
//! before the green commit has passed the post-land tests. Velocity reads that from
//! Rosen instead of re-deriving it from per-commit statuses.

use crate::{ProjectServer, RosenError};

/// The commit to release and the green commit that covers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseCommit {
    pub commit: String,
    pub green: String,
}

/// `requested` when Rosen's last green commit covers it, or the green commit itself when
/// no commit is requested. A commit outside the green prefix is refused.
pub async fn release_commit(
    server: &ProjectServer,
    requested: Option<&str>,
) -> Result<ReleaseCommit, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    // Offers are idempotent, so reading the green prefix this way claims no work.
    let green = client
        .post_land_next()
        .await?
        .last_green
        .ok_or_else(|| {
            RosenError::Release(
                "Rosen has no green commit yet: the post-land tests have not passed on any land"
                    .into(),
            )
        })?
        .to_string();
    let Some(requested) = requested.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(ReleaseCommit {
            commit: green.clone(),
            green,
        });
    };
    let identifier = crate::policy::object(requested)?;
    if client.merge_base(requested, &green).await?.merge_base != Some(identifier) {
        return Err(RosenError::Release(format!(
            "{requested} is not covered by Rosen's last green commit {green}. Release a landed commit the post-land tests have passed on."
        )));
    }
    Ok(ReleaseCommit {
        commit: identifier.to_string(),
        green,
    })
}
