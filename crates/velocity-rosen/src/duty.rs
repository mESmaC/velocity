//! Before-land review operations on a Rosen change (Rosen R7, R8; Velocity F7).

use rosen_client::protocol::{CommentTarget, Diff, DiffRequest};

use crate::policy::object;
use crate::workspace::session;
use crate::{ProjectServer, RosenError};

pub use crate::change_details::{
    ChangeOverview, ReviewThreadSummary, ReviewTrees, change_overview, changed_paths, diff_stat,
};
pub use crate::checkout::{MergeCheckout, check_out_merge_result, remove_checkout};
pub use crate::landing::{LandOutcome, land};

/// A change as PR duty needs it to decide what it needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeStanding {
    pub head: String,
    /// Open or ready: neither landed nor abandoned.
    pub open: bool,
    /// A valid approval stands at exactly `head`.
    pub approved: bool,
}

/// Where `change`'s review stands.
pub async fn change_standing(
    server: &ProjectServer,
    change: &str,
) -> Result<ChangeStanding, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let record = client.change_show(change).await?.change;
    let head = record.head;
    let approved = client
        .review_approvals(change)
        .await?
        .iter()
        .any(|approval| approval.valid && approval.head == head);
    Ok(ChangeStanding {
        head: head.to_string(),
        open: !record.state.is_closed(),
        approved,
    })
}

/// The change's diff against its parent, with no unchanged lines.
pub async fn change_diff_record(server: &ProjectServer, change: &str) -> Result<Diff, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    Ok(client
        .diff(DiffRequest {
            to: Some(change.to_owned()),
            context: Some(0),
            ..DiffRequest::default()
        })
        .await?)
}

/// Approves `change` at `head` as `reviewer`. Rosen refuses a head that is no longer the
/// change's.
pub async fn approve(
    server: &ProjectServer,
    reviewer: &str,
    change: &str,
    head: &str,
) -> Result<(), RosenError> {
    session(server, reviewer)
        .await?
        .approve(change, object(head)?)
        .await?;
    Ok(())
}

/// Starts a review thread on `line` of `path` at the change's head.
pub async fn comment_on_line(
    server: &ProjectServer,
    reviewer: &str,
    change: &str,
    (path, line, body): (&str, u32, &str),
) -> Result<(), RosenError> {
    let target = CommentTarget::Line {
        path: path.to_owned(),
        line,
    };
    session(server, reviewer)
        .await?
        .comment(change, target, body, None)
        .await?;
    Ok(())
}

/// Records that the pre-land checks passed on `change`'s merge result of `head` onto
/// `base`: the receipt Rosen needs to land it. The receipt names the check set `base`
/// holds, the one Rosen asks for when it lands on that base, not whatever set the current
/// base names by the time the checks finish.
pub async fn submit_receipt(
    server: &ProjectServer,
    reviewer: &str,
    change: &str,
    (head, base): (&str, &str),
) -> Result<(), RosenError> {
    let check_set = crate::policy::policy_at(server, Some(base))
        .await?
        .check_set;
    session(server, reviewer)
        .await?
        .submit_receipt(change, object(head)?, object(base)?, &check_set)
        .await?;
    Ok(())
}
