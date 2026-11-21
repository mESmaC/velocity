//! Automatically ticks the owning §7 implementation-order item in Rosen when a change lands (#539).

use rosen_client::protocol::{ErrorCode, ReadAnswer, SnapshotRequest};
use rosen_client::{Client, Error, ObjectId};
use rosen_core::ReviewMode;
pub use velocity_spec::TickQuery;
use velocity_spec::tick_implementation_item;

use crate::connection::requests;
use crate::landing::{self, LandOutcome, LandRefusal};
use crate::workspace::session;
use crate::{ProjectServer, RosenError};

/// Agent identity used for Velocity's automatic §7 checkbox ticks on land.
pub const SPEC_TICK_AGENT: &str = "velocity-spec";

const SPLIT_PLAN_PATH: &str = "spec/7-implementation-order.md";

/// Outcome of attempting to tick a §7 item on land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TickOutcome {
    /// No unchecked §7 item matched the landed work (or it was already ticked).
    NoMatch,
    /// Ticked the item and landed the single-line spec update.
    Ticked {
        path: String,
        item: String,
        commit: String,
    },
    /// Project policy or Rosen refused automatic landing of the tick change.
    Refused(String),
}

/// Finds and ticks the owning unchecked §7 item for a landed change, landing the single-line
/// edit on after-land Rosen projects so agents do not edit SPEC.md §7 in every change (#539).
pub async fn tick_on_land(
    server: &ProjectServer,
    spec_path: &str,
    query: &TickQuery<'_>,
    label: &str,
) -> Result<TickOutcome, RosenError> {
    if query.is_empty() {
        return Ok(TickOutcome::NoMatch);
    }
    let client = requests(server.endpoint()).await?;
    if client.project_config(None).await?.config.review != ReviewMode::AfterLand {
        return Ok(TickOutcome::Refused(
            "project does not use after-land review".into(),
        ));
    }
    let session = session(server, SPEC_TICK_AGENT).await?;
    let record = session.new_change(None).await?;
    let change = record.change.id.to_string();
    let outcome = run_tick(
        &client,
        server,
        &change,
        record.change.head,
        spec_path,
        query,
        label,
    )
    .await;
    if !matches!(outcome, Ok(TickOutcome::Ticked { .. })) {
        let _ = session.abandon(&change).await;
    }
    outcome
}

async fn run_tick(
    client: &Client,
    server: &ProjectServer,
    change: &str,
    starting_tree: ObjectId,
    spec_path: &str,
    query: &TickQuery<'_>,
    label: &str,
) -> Result<TickOutcome, RosenError> {
    let candidates = candidate_paths(spec_path);
    let mut matched = None;
    for path in candidates {
        let Some(content) = read_file(client, starting_tree, &path).await? else {
            continue;
        };
        if let Some((updated, item)) = tick_implementation_item(&content, query) {
            matched = Some((path, updated, item));
            break;
        }
    }
    let Some((path, updated, item)) = matched else {
        return Ok(TickOutcome::NoMatch);
    };
    let written = client
        .workspace_write(change, &path, Some(updated.into_bytes()))
        .await?;
    let head = written.change.change.head;
    let message = format!("Tick §7 item on land ({label})");
    client
        .snapshot(SnapshotRequest {
            change: change.to_owned(),
            tree: head,
            from: Some(head),
            message: Some(message),
        })
        .await?;
    match landing::land(server, SPEC_TICK_AGENT, change).await? {
        LandOutcome::Landed { commit } => Ok(TickOutcome::Ticked { path, item, commit }),
        LandOutcome::Refused(refusal) => Ok(TickOutcome::Refused(refusal_summary(refusal))),
    }
}

fn candidate_paths(spec_path: &str) -> Vec<String> {
    let mut paths = vec![SPLIT_PLAN_PATH.to_owned()];
    let normalized = spec_path.replace('\\', "/");
    let trimmed = normalized.trim();
    if !trimmed.is_empty() && !paths.iter().any(|existing| existing == trimmed) {
        paths.push(trimmed.to_owned());
    }
    if !paths
        .iter()
        .any(|existing| existing == velocity_spec::ROOT_FILE)
    {
        paths.push(velocity_spec::ROOT_FILE.to_owned());
    }
    paths
}

async fn read_file(
    client: &Client,
    tree: ObjectId,
    path: &str,
) -> Result<Option<String>, RosenError> {
    match client.workspace_read(&tree.to_string(), path).await {
        Ok(ReadAnswer::File { mode, data, .. }) if matches!(mode.as_str(), "100644" | "100755") => {
            Ok(String::from_utf8(data).ok())
        }
        Ok(_) => Ok(None),
        Err(Error::Server(error)) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn refusal_summary(refusal: LandRefusal) -> String {
    match refusal {
        LandRefusal::Regenerate { paths } => format!("regenerate {}", paths.join(", ")),
        LandRefusal::Conflict { message, .. } | LandRefusal::Other { message, .. } => message,
    }
}
