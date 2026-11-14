//! PR duty's operations on a Rosen change (Rosen R7, R8; Velocity F7): where its review
//! stands, its merge result checked out to build in, the approval, the receipt and the land.
//! Rosen enforces the gate itself: it lands nothing without an approval at the change's
//! current head and a receipt for its merge result.

use std::path::{Path, PathBuf};

use rosen_client::Client;
use rosen_client::protocol::{CommentTarget, DiffRequest, LandStatus};

use crate::policy::object;
use crate::workspace::session;
use crate::{ProjectServer, RosenError};

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
    let client = Client::connect(server.endpoint()).await?;
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

/// What a reviewer reads about a change: its named snapshots and its review threads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeOverview {
    pub head: String,
    pub owner: String,
    /// The messages of its `rosen commit -m` snapshots, oldest first: the author's own
    /// description of the work.
    pub named: Vec<String>,
    pub threads: Vec<ReviewThreadSummary>,
}

/// One review thread on a change, with its comments as `author: body`, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewThreadSummary {
    pub path: String,
    pub line: u32,
    pub resolved: bool,
    pub comments: Vec<String>,
}

/// `change`'s named snapshots and review threads.
pub async fn change_overview(
    server: &ProjectServer,
    change: &str,
) -> Result<ChangeOverview, RosenError> {
    use rosen_client::protocol::ThreadFilter;
    use rosen_core::{SnapshotKind, ThreadState};
    let client = Client::connect(server.endpoint()).await?;
    let record = client.change_show(change).await?.change;
    let named = record
        .snapshots
        .into_iter()
        .filter_map(|snapshot| match snapshot.kind {
            SnapshotKind::Named(message) => Some(message),
            SnapshotKind::Automatic => None,
        })
        .collect();
    let filter = ThreadFilter {
        change: change.to_owned(),
        state: None,
    };
    let threads = client
        .review_list(filter)
        .await?
        .into_iter()
        .map(|thread| ReviewThreadSummary {
            path: thread.path,
            line: thread.line,
            resolved: thread.state == ThreadState::Resolved,
            comments: thread
                .comments
                .into_iter()
                .map(|comment| format!("{}: {}", comment.author, comment.body))
                .collect(),
        })
        .collect();
    Ok(ChangeOverview {
        head: record.head.to_string(),
        owner: record.owner.to_string(),
        named,
        threads,
    })
}

/// A change's merge result, checked out to build in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeCheckout {
    pub head: String,
    pub base: String,
    /// Files that did not merge; the folder is left empty when there are any.
    pub conflicts: Vec<String>,
}

/// Writes `change`'s current merge result onto the base into `folder`, replacing what an
/// earlier checkout left there. A folder Velocity did not check out is never touched.
///
/// The folder itself stays: it is the reviewing agent's working directory, which Windows
/// won't remove. Rosen writes the result into a new sibling, whose entries then replace the
/// folder's.
///
/// With `prepared`, for a review that builds, the result comes from Rosen's warm pool when
/// it can (Rosen R3): a ready workspace brought to the merge result, with the build output
/// its prepare left, so the review's first build compiles what the change touched and not
/// the whole project. A review that only reads asks for the files alone, and takes nothing
/// from the pool.
pub async fn check_out_merge_result(
    server: &ProjectServer,
    reviewer: &str,
    change: &str,
    folder: &Path,
    prepared: bool,
) -> Result<MergeCheckout, RosenError> {
    let staging = claim_checkout(folder)?;
    let session = session(server, reviewer).await?;
    let checkout = if prepared {
        session
            .check_out_prepared_merge_result(change, &staging)
            .await?
    } else {
        session.check_out_merge_result(change, &staging).await?
    };
    replace_entries(folder, &staging)?;
    Ok(MergeCheckout {
        head: checkout.merge.head.to_string(),
        base: checkout.merge.base.to_string(),
        conflicts: checkout.merge.conflicts,
    })
}

/// Makes `folder` a checkout of Velocity's, with nothing staged beside it, and returns
/// where the next merge result is staged.
pub(crate) fn claim_checkout(folder: &Path) -> Result<PathBuf, RosenError> {
    let marker = checkout_marker(folder);
    let staging = staging_folder(folder);
    // The marker is what makes the folder and its staging sibling Velocity's: without it,
    // either one belongs to someone else and is left alone.
    for path in [folder, staging.as_path()] {
        if path.exists() && !marker.is_file() {
            return Err(RosenError::Io {
                action: "replace",
                path: path.to_path_buf(),
                source: std::io::Error::other("the folder is not a Rosen review checkout"),
            });
        }
    }
    if staging.exists() {
        remove_folder(&staging)?;
    }
    create_folder(folder)?;
    write_marker(&marker)?;
    Ok(staging)
}

/// Where a merge result is written before it replaces what `folder` holds.
pub(crate) fn staging_folder(folder: &Path) -> PathBuf {
    sibling(folder, ".rosen-next")
}

/// Empties `folder`, moves every entry of `staging` into it and removes `staging`.
fn replace_entries(folder: &Path, staging: &Path) -> Result<(), RosenError> {
    for entry in entries(folder)? {
        let path = entry.path();
        let removed = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.map_err(|source| io_error("remove", &path, source))?;
    }
    // Rosen leaves no folder when the merge conflicts.
    if !staging.exists() {
        return Ok(());
    }
    for entry in entries(staging)? {
        let target = folder.join(entry.file_name());
        std::fs::rename(entry.path(), &target)
            .map_err(|source| io_error("move", &target, source))?;
    }
    remove_folder(staging)
}

fn entries(folder: &Path) -> Result<Vec<std::fs::DirEntry>, RosenError> {
    std::fs::read_dir(folder)
        .and_then(Iterator::collect)
        .map_err(|source| io_error("read", folder, source))
}

fn create_folder(folder: &Path) -> Result<(), RosenError> {
    std::fs::create_dir_all(folder).map_err(|source| io_error("create", folder, source))
}

fn io_error(action: &'static str, path: &Path, source: std::io::Error) -> RosenError {
    RosenError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// Removes a review checkout and its marker, once its agent has stopped.
pub fn remove_checkout(folder: &Path) -> Result<(), RosenError> {
    let marker = checkout_marker(folder);
    if !marker.is_file() {
        return Ok(());
    }
    for path in [folder.to_path_buf(), staging_folder(folder)] {
        if path.exists() {
            remove_folder(&path)?;
        }
    }
    let _ = std::fs::remove_file(marker);
    Ok(())
}

/// Beside the folder, not in it: the folder is the build tree the checks run on.
fn checkout_marker(folder: &Path) -> PathBuf {
    sibling(folder, ".rosen-review")
}

pub(crate) fn sibling(folder: &Path, suffix: &str) -> PathBuf {
    let mut name = folder.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    folder.with_file_name(name)
}

fn write_marker(marker: &Path) -> Result<(), RosenError> {
    std::fs::write(marker, b"").map_err(|source| RosenError::Io {
        action: "write",
        path: marker.to_path_buf(),
        source,
    })
}

fn remove_folder(folder: &Path) -> Result<(), RosenError> {
    std::fs::remove_dir_all(folder).map_err(|source| RosenError::Io {
        action: "remove",
        path: folder.to_path_buf(),
        source,
    })
}

/// The paths `change` touches against its parent, for its review tier.
pub async fn changed_paths(
    server: &ProjectServer,
    change: &str,
) -> Result<Vec<String>, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let request = DiffRequest {
        to: Some(change.to_owned()),
        context: Some(0),
        ..DiffRequest::default()
    };
    Ok(client
        .diff(request)
        .await?
        .files
        .into_iter()
        .map(|file| file.path)
        .collect())
}

/// The change's diff against its parent, with three lines of context, as unified text.
pub async fn change_diff(server: &ProjectServer, change: &str) -> Result<String, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let request = DiffRequest {
        to: Some(change.to_owned()),
        ..DiffRequest::default()
    };
    Ok(unified(&client.diff(request).await?))
}

/// The diff as unified text, the form agents read best.
pub(crate) fn unified(diff: &rosen_client::protocol::Diff) -> String {
    use rosen_client::protocol::LineKind;
    let mut text = String::new();
    for file in &diff.files {
        text.push_str(&format!("--- a/{0}\n+++ b/{0}\n", file.path));
        if file.binary {
            text.push_str("Binary file differs\n");
            continue;
        }
        for hunk in &file.hunks {
            text.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
            ));
            for line in &hunk.lines {
                let sign = match line.kind {
                    LineKind::Context => ' ',
                    LineKind::Added => '+',
                    LineKind::Removed => '-',
                };
                text.push(sign);
                text.push_str(&line.text);
                text.push('\n');
            }
        }
    }
    text
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

/// What a land request came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandOutcome {
    Landed {
        commit: String,
    },
    /// Refused, with Rosen's stable code (`unapproved`, `receipt_stale`, `needs_recheck`,
    /// `merge_conflict`, ...) and its advice.
    Refused {
        code: String,
        message: String,
    },
}

/// Asks Rosen to land `change`.
pub async fn land(
    server: &ProjectServer,
    reviewer: &str,
    change: &str,
) -> Result<LandOutcome, RosenError> {
    let answer = session(server, reviewer).await?.land(change).await?;
    Ok(match answer.outcome {
        LandStatus::Landed { commit, .. } | LandStatus::AlreadyLanded { commit } => {
            LandOutcome::Landed {
                commit: commit.to_string(),
            }
        }
        LandStatus::Refused {
            reason, message, ..
        } => LandOutcome::Refused {
            code: reason,
            message,
        },
    })
}
