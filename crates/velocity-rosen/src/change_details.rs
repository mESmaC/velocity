//! Change metadata and diff views shared by review sessions.

use rosen_client::protocol::{Diff, DiffRequest, FileDiff, LineKind};
use serde_json::{Value, json};

use crate::{ProjectServer, RosenError};

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
    let client = crate::connection::requests(server.endpoint()).await?;
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

/// The paths `change` touches against its parent, for its review tier.
pub async fn changed_paths(
    server: &ProjectServer,
    change: &str,
) -> Result<Vec<String>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
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

/// The immutable trees behind an overview. Later pages keep using these trees
/// even when an automatic snapshot or rebase moves the change.
pub struct ReviewTrees {
    base: String,
    head: String,
}

impl ReviewTrees {
    pub fn from_diff(diff: &Diff) -> Self {
        Self {
            base: diff.from.to_string(),
            head: diff.to.to_string(),
        }
    }

    pub async fn file(
        &self,
        server: &ProjectServer,
        path: &str,
    ) -> Result<Option<String>, RosenError> {
        let client = crate::connection::requests(server.endpoint()).await?;
        let diff = client
            .diff(DiffRequest {
                from: Some(self.base.clone()),
                to: Some(self.head.clone()),
                paths: vec![path.to_owned()],
                ..DiffRequest::default()
            })
            .await?;
        Ok(diff
            .files
            .iter()
            .find(|file| file.path == path)
            .map(file_patch))
    }
}

pub fn diff_stat(diff: &Diff) -> Value {
    let files: Vec<_> = diff
        .files
        .iter()
        .map(|file| {
            let mut added = 0;
            let mut removed = 0;
            for line in file.hunks.iter().flat_map(|hunk| &hunk.lines) {
                match line.kind {
                    LineKind::Added => added += 1,
                    LineKind::Removed => removed += 1,
                    LineKind::Context => {}
                }
            }
            json!({"path":file.path, "status":file.status, "added":added, "removed":removed,
            "binary":file.binary})
        })
        .collect();
    json!(files)
}

pub(crate) fn file_patch(file: &FileDiff) -> String {
    let mut text = String::new();
    let old = file.old.as_ref().map(|side| side.mode.as_str());
    let new = file.new.as_ref().map(|side| side.mode.as_str());
    match (old, new) {
        (None, Some(mode)) => text.push_str(&format!("new file mode {mode}\n")),
        (Some(mode), None) => text.push_str(&format!("deleted file mode {mode}\n")),
        (Some(old), Some(new)) if old != new => {
            text.push_str(&format!("old mode {old}\nnew mode {new}\n"));
        }
        _ => {}
    }
    let old_path = old.map_or_else(|| "/dev/null".to_owned(), |_| format!("a/{}", file.path));
    let new_path = new.map_or_else(|| "/dev/null".to_owned(), |_| format!("b/{}", file.path));
    text.push_str(&format!("--- {old_path}\n+++ {new_path}\n"));
    if file.binary {
        text.push_str("Binary file differs\n");
        return text;
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
    text
}
