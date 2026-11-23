//! Rosen inspection, receipt recording, gate verification and Cargo workspace discovery
//! for `preflight` (#537, #560).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rosen_client::protocol::{DiffRequest, OverlapQuery, SubmitReceipt};
use rosen_client::snapshot::WorkspaceMarker;
use rosen_client::{AgentId, ChangeState, Client, Endpoint, ObjectId};
use rosen_core::ReviewMode;
use rusqlite::{Connection as SqliteConnection, OpenFlags, params};
use toml_edit::{DocumentMut, Item, Value};
use velocity_core::preflight::{
    PreflightOverlap, PreflightRecord, WorkspaceCrate, evaluate_gate, failed_non_overlap_checks,
    failed_on_overlaps, land_refusal_parts, name_refusal_message, normalize_path,
};

pub use rosen_client::protocol::OverlapRecord;

use crate::connection::Connection;
use crate::landing::LandRefusal;
use crate::{ProjectServer, RosenError};

/// Rosen receipt submitter nick recorded when `preflight` passes on an `after-land` project.
pub const PREFLIGHT_SUBMITTER: &str = "velocity-preflight";

const PREFLIGHT_STORE_DIR: &str = "velocity-preflight";
const MAX_GLOB_SCAN_DEPTH: usize = 3;
const SKIPPED_SCAN_FOLDERS: &[&str] = &[".git", ".rosen", "node_modules", "proofs", "target"];

/// One changed path in a Rosen diff against its parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    /// True when the file is present in the new tree (added or modified).
    pub exists: bool,
}

/// Authoritative Rosen state collected at the start of a preflight run.
pub struct PreflightSnapshot {
    pub change: String,
    pub head: ObjectId,
    pub workspace_tree: ObjectId,
    pub base: ObjectId,
    /// Parent tree (`diff.from`) that `files` is computed against (#737).
    pub parent_tree: ObjectId,
    /// Target tree (`diff.to`) returned by Rosen's diff (#737).
    pub diff_tree: ObjectId,
    pub review_mode: ReviewMode,
    pub pre_land_check_set: String,
    pub data_dir: Option<PathBuf>,
    pub files: Vec<DiffFile>,
    pub land_refusal: Option<LandRefusal>,
    pub unreadable: Vec<String>,
    pub overlaps: Vec<OverlapRecord>,
    pub client: Arc<Connection>,
}

/// Outcome of recording a completed preflight run against Rosen and Velocity's store (#560).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordOutcome {
    Recorded {
        receipt_submitted: bool,
    },
    /// The change moved while preflight was running, so the checked snapshot is stale.
    StaleHead {
        checked: String,
        current: String,
    },
}

/// Outcome of checking the preflight precondition before landing a named change (#560).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandPreflightGate {
    /// The change is already landed or abandoned.
    Closed,
    /// The change's latest snapshot has a passing preflight receipt or record.
    Passed,
    /// The change's latest snapshot has no passing preflight.
    Refused {
        reason: String,
        instructions: String,
    },
}

/// Flushes local workspace edits to Rosen and inspects the change's diff, head, base policy,
/// merge/regeneration status, and active overlaps.
pub async fn inspect(endpoint: &str, worktree: &Path) -> Result<PreflightSnapshot, RosenError> {
    let marker = WorkspaceMarker::read(worktree).map_err(|source| RosenError::Io {
        action: "read the Rosen marker of",
        path: worktree.to_path_buf(),
        source,
    })?;
    let change = marker.change.to_string();
    let client = crate::connection::requests(&Endpoint::new(endpoint)).await?;
    let report = client.snapshot_workspace(worktree, None).await?;
    let diff_request = DiffRequest {
        to: Some(change.clone()),
        context: Some(0),
        ..DiffRequest::default()
    };
    let overlap_query = OverlapQuery {
        change: Some(change.clone()),
        paths: Vec::new(),
    };
    let (diff, config, land_check, overlaps, status) = tokio::try_join!(
        client.diff(diff_request),
        client.project_config(None),
        client.land_check(&change),
        client.overlaps(overlap_query),
        client.status(),
    )?;
    let config = if config.commit == land_check.base {
        config
    } else {
        client.project_config(Some(land_check.base)).await?
    };
    let parent_tree = diff.from;
    let diff_tree = diff.to;
    let files = diff
        .files
        .into_iter()
        .map(|file| DiffFile {
            path: file.path,
            exists: file.new.is_some(),
        })
        .collect();
    Ok(PreflightSnapshot {
        change,
        head: land_check.head,
        workspace_tree: report.tree,
        base: land_check.base,
        parent_tree,
        diff_tree,
        review_mode: config.config.review,
        pre_land_check_set: config.config.checks.pre_land.name,
        data_dir: local_data_dir(&status.data_dir),
        files,
        land_refusal: land_check.refusal.map(map_land_refusal),
        unreadable: report.unreadable,
        overlaps: overlaps.overlaps,
        client,
    })
}

fn map_land_refusal(refusal: rosen_client::protocol::LandRefusal) -> LandRefusal {
    match refusal.reason.as_str() {
        "regeneration_required" => LandRefusal::Regenerate {
            paths: refusal.paths,
        },
        "merge_conflict" | "conflicts" => LandRefusal::Conflict {
            paths: refusal.paths,
            message: refusal.message,
        },
        _ => LandRefusal::Other {
            code: refusal.reason,
            message: refusal.message,
        },
    }
}

/// Records the outcome of a preflight run for `snapshot.head` (#560).
///
/// On `after-land` projects, a passing preflight also submits a signed Rosen receipt bound
/// to `(change, head, base, check_set, submitter = "velocity-preflight")` so Rosen carries
/// the attestation directly on the snapshot. On `before-land` projects, receipts are left
/// untouched because Rosen reserves them for the full `[checks.pre_land]` gate.
pub async fn record_outcome(
    snapshot: &PreflightSnapshot,
    passed: bool,
    failure_summary: &str,
    log_path: &str,
) -> Result<RecordOutcome, RosenError> {
    let current = snapshot.client.change_show(&snapshot.change).await?.change;
    if current.head != snapshot.head {
        return Ok(RecordOutcome::StaleHead {
            checked: snapshot.head.to_string(),
            current: current.head.to_string(),
        });
    }
    let receipt_submitted = if passed && snapshot.review_mode == ReviewMode::AfterLand {
        match submit_preflight_receipt(snapshot).await? {
            ReceiptAttempt::Submitted => true,
            ReceiptAttempt::Skipped => false,
            ReceiptAttempt::StaleHead(current_head) => {
                return Ok(RecordOutcome::StaleHead {
                    checked: snapshot.head.to_string(),
                    current: current_head,
                });
            }
        }
    } else {
        false
    };
    let overlaps_failed = !passed && failed_on_overlaps(failure_summary);
    let blocking_overlaps = if overlaps_failed {
        snapshot
            .overlaps
            .iter()
            .map(|overlap| PreflightOverlap {
                change: overlap.change.to_string(),
                owner: overlap.owner.to_string(),
                paths: overlap.paths.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };
    let non_overlap_checks_passed = passed || !failed_non_overlap_checks(failure_summary);
    let record = PreflightRecord {
        change: snapshot.change.clone(),
        head: snapshot.head.to_string(),
        base: snapshot.base.to_string(),
        passed,
        failure_summary: if passed {
            String::new()
        } else {
            failure_summary.to_owned()
        },
        blocking_overlaps,
        non_overlap_checks_passed,
        log_path: log_path.to_owned(),
        recorded_at: unix_now(),
    };
    let data_dir = snapshot.data_dir.clone();
    tokio::task::spawn_blocking(move || save_record(data_dir.as_deref(), &record))
        .await
        .map_err(|error| RosenError::StatusData(error.to_string()))??;
    Ok(RecordOutcome::Recorded { receipt_submitted })
}

/// Reads the persisted preflight record for `change` from `server`'s data directory (#565).
pub fn load_change_record(server: &ProjectServer, change: &str) -> Option<PreflightRecord> {
    load_record(Some(server.data_dir()), change)
}

/// Returns the subset of `changes` that are still `ChangeState::Open` on `server` (#565).
/// A missing change on the server is treated as closed; transport errors are propagated.
pub async fn open_change_ids(
    server: &ProjectServer,
    changes: &[String],
) -> Result<BTreeSet<String>, RosenError> {
    if changes.is_empty() {
        return Ok(BTreeSet::new());
    }
    let client = crate::connection::requests(server.endpoint()).await?;
    let mut open = BTreeSet::new();
    for change in changes {
        match client.change_show(change).await {
            Ok(shown) if shown.change.state == ChangeState::Open => {
                open.insert(change.clone());
            }
            Ok(_) | Err(rosen_client::Error::Server(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(open)
}

/// Returns `(preflight_passed_for_head, has_passing_work)` for `(change, head)` (#565).
pub async fn passing_work_status(
    server: &ProjectServer,
    change: &str,
    head: &str,
) -> Result<(bool, bool), RosenError> {
    let data_dir = server.data_dir().to_path_buf();
    let change_owned = change.to_owned();
    let head_owned = head.to_owned();
    tokio::task::spawn_blocking(move || {
        let receipt = has_preflight_receipt(&data_dir, &change_owned, &head_owned)?;
        let record = load_record(Some(&data_dir), &change_owned);
        let passed_for_head = receipt
            || record
                .as_ref()
                .is_some_and(|saved| saved.passed && saved.head == head_owned);
        let has_passing_work = passed_for_head
            || record
                .as_ref()
                .is_some_and(PreflightRecord::has_passing_work);
        Ok::<_, RosenError>((passed_for_head, has_passing_work))
    })
    .await
    .map_err(|error| RosenError::StatusData(error.to_string()))?
}

enum ReceiptAttempt {
    Submitted,
    Skipped,
    StaleHead(String),
}

async fn submit_preflight_receipt(
    snapshot: &PreflightSnapshot,
) -> Result<ReceiptAttempt, RosenError> {
    let submitter = AgentId::new(PREFLIGHT_SUBMITTER)
        .map_err(|error| RosenError::AgentName(error.to_string()))?;
    let request = SubmitReceipt {
        change: snapshot.change.clone(),
        head: snapshot.head,
        base: snapshot.base,
        check_set: snapshot.pre_land_check_set.clone(),
        submitter,
        signature: None,
    };
    match snapshot.client.submit_receipt(request).await {
        Ok(_) => Ok(ReceiptAttempt::Submitted),
        Err(rosen_client::Error::Server(_)) => {
            let latest = snapshot.client.change_show(&snapshot.change).await?.change;
            if latest.head != snapshot.head {
                Ok(ReceiptAttempt::StaleHead(latest.head.to_string()))
            } else {
                Ok(ReceiptAttempt::Skipped)
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Verifies that `change`'s latest snapshot on `server` has a passing preflight before
/// Velocity lands it (#560).
pub async fn verify_for_land(
    server: &ProjectServer,
    change: &str,
) -> Result<LandPreflightGate, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let shown = client.change_show(change).await?.change;
    if shown.state.is_closed() {
        return Ok(LandPreflightGate::Closed);
    }
    let head = shown.head.to_string();
    let data_dir = server.data_dir().to_path_buf();
    let change_owned = change.to_owned();
    let head_owned = head.clone();
    let (has_receipt, stored) = tokio::task::spawn_blocking(move || {
        let receipt = has_preflight_receipt(&data_dir, &change_owned, &head_owned)?;
        let record = load_record(Some(&data_dir), &change_owned);
        Ok::<_, RosenError>((receipt, record))
    })
    .await
    .map_err(|error| RosenError::StatusData(error.to_string()))??;
    let decision = evaluate_gate(&head, has_receipt, stored.as_ref());
    Ok(match land_refusal_parts(change, &head, &decision) {
        None => LandPreflightGate::Passed,
        Some((reason, instructions)) => LandPreflightGate::Refused {
            reason,
            instructions,
        },
    })
}

/// Flushes local workspace edits and verifies that the resulting snapshot passed preflight
/// before naming the change (#560). Returns `Ok(Ok(()))` when naming may proceed, or
/// `Ok(Err(message))` with the refusal explaining what failed.
pub async fn verify_workspace_for_commit(
    endpoint: &str,
    workspace: &Path,
) -> Result<Result<(), String>, RosenError> {
    let Some(root) = WorkspaceMarker::find_root(workspace) else {
        return Ok(Ok(()));
    };
    let marker = WorkspaceMarker::read(&root).map_err(|source| RosenError::Io {
        action: "read the Rosen marker of",
        path: root.clone(),
        source,
    })?;
    let client = Client::connect(&Endpoint::new(endpoint)).await?;
    verify_workspace_for_commit_with_client(&client, &root, &marker.change.to_string()).await
}

/// Same check as [`verify_workspace_for_commit`] using an already-connected `Client`.
pub(crate) async fn verify_workspace_for_commit_with_client(
    client: &Client,
    root: &Path,
    change: &str,
) -> Result<Result<(), String>, RosenError> {
    let report = client.snapshot_workspace(root, None).await?;
    let short_change = change.get(..8).unwrap_or(change);
    if !report.unreadable.is_empty() {
        return Ok(Err(format!(
            "Cannot name change {short_change}: some files are held open ({}). \
             Close or save them, run `preflight` from `velocity-run`, and try again.",
            report.unreadable.join(", ")
        )));
    }
    let (shown, status) = tokio::try_join!(client.change_show(change), client.status())?;
    if shown.change.state.is_closed() {
        return Ok(Ok(()));
    }
    let head = shown.change.head.to_string();
    if report.tree != shown.change.head {
        let workspace_tree = report.tree.to_string();
        return Ok(Err(format!(
            "Cannot name change {short_change}: the workspace is behind a server rebase \
             (workspace {}, change head {}). Run `rosen rebase`, run `preflight` from \
             `velocity-run` until it passes, and then name the change.",
            workspace_tree.get(..8).unwrap_or(&workspace_tree),
            head.get(..8).unwrap_or(&head),
        )));
    }
    let data_dir = local_data_dir(&status.data_dir);
    let change_owned = change.to_owned();
    let head_owned = head.clone();
    let (has_receipt, stored) = tokio::task::spawn_blocking(move || {
        let receipt = match data_dir.as_deref() {
            Some(dir) => has_preflight_receipt(dir, &change_owned, &head_owned)?,
            None => false,
        };
        let record = load_record(data_dir.as_deref(), &change_owned);
        Ok::<_, RosenError>((receipt, record))
    })
    .await
    .map_err(|error| RosenError::StatusData(error.to_string()))??;
    let decision = evaluate_gate(&head, has_receipt, stored.as_ref());
    Ok(match name_refusal_message(change, &head, &decision) {
        None => Ok(()),
        Some(message) => Err(message),
    })
}

/// Enforces the preflight precondition for `rosen commit` invocations reaching Velocity's
/// agent `rosen` shim (#560).
pub(crate) fn verify_cli_commit(arguments: &[OsString], subcommand: usize) -> io::Result<()> {
    let commit_args = &arguments[subcommand + 1..];
    if is_info_invocation(commit_args) {
        return Ok(());
    }
    let Some(endpoint) = cli_endpoint(arguments) else {
        return Ok(());
    };
    let start = match cli_workspace(commit_args) {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    if WorkspaceMarker::find_root(&start).is_none() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    match runtime.block_on(verify_workspace_for_commit(&endpoint, &start)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(refusal)) => Err(io::Error::other(refusal)),
        Err(error) => Err(io::Error::other(format!(
            "Could not verify preflight before naming the change: {error}"
        ))),
    }
}

fn is_info_invocation(arguments: &[OsString]) -> bool {
    arguments.iter().any(|argument| {
        matches!(
            argument.to_str(),
            Some("--help" | "-h" | "--version" | "-V" | "--json-schema")
        )
    })
}

fn cli_endpoint(arguments: &[OsString]) -> Option<String> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).and_then(|value| value.to_str()) {
        if argument == "--endpoint" {
            return arguments
                .get(index + 1)
                .and_then(|value| value.to_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned);
        }
        if let Some(value) = argument.strip_prefix("--endpoint=")
            && !value.trim().is_empty()
        {
            return Some(value.to_owned());
        }
        index += 1;
    }
    std::env::var("ROSEN_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn cli_workspace(arguments: &[OsString]) -> Option<PathBuf> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--workspace" {
            return arguments.get(index + 1).map(PathBuf::from);
        }
        if let Some(text) = argument.to_str()
            && let Some(path) = text.strip_prefix("--workspace=")
            && !path.is_empty()
        {
            return Some(PathBuf::from(path));
        }
        index += 1;
    }
    None
}

fn local_data_dir(raw: &str) -> Option<PathBuf> {
    if raw.trim().is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    path.is_dir().then_some(path)
}

fn record_store_dir(data_dir: Option<&Path>) -> PathBuf {
    match data_dir {
        Some(dir) => dir.join(PREFLIGHT_STORE_DIR),
        None => std::env::temp_dir().join(PREFLIGHT_STORE_DIR),
    }
}

fn record_file(data_dir: Option<&Path>, change: &str) -> PathBuf {
    let safe: String = change
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .collect();
    let stem = if safe.is_empty() { "unknown" } else { &safe };
    record_store_dir(data_dir).join(format!("{stem}.json"))
}

fn save_record(data_dir: Option<&Path>, record: &PreflightRecord) -> Result<(), RosenError> {
    let destination = record_file(data_dir, &record.change);
    let Some(parent) = destination.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|source| RosenError::Io {
        action: "create preflight state folder",
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|error| RosenError::StatusData(error.to_string()))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| RosenError::Io {
            action: "create temporary preflight record in",
            path: parent.to_path_buf(),
            source,
        })?;
    io::Write::write_all(&mut temporary, &bytes).map_err(|source| RosenError::Io {
        action: "write preflight record to",
        path: destination.clone(),
        source,
    })?;
    temporary
        .persist(&destination)
        .map_err(|error| RosenError::Io {
            action: "persist preflight record at",
            path: destination,
            source: error.error,
        })?;
    Ok(())
}

fn load_record(data_dir: Option<&Path>, change: &str) -> Option<PreflightRecord> {
    let path = record_file(data_dir, change);
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Checks Rosen's read-only metadata ledger for a signed preflight receipt on `(change, head)`.
fn has_preflight_receipt(data_dir: &Path, change: &str, head: &str) -> Result<bool, RosenError> {
    let database = data_dir.join("metadata.sqlite");
    if !database.is_file() {
        return Ok(false);
    }
    let connection = SqliteConnection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'receipts')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(false);
    }
    let has_signature: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('receipts') WHERE name = 'signature')",
        [],
        |row| row.get(0),
    )?;
    if !has_signature {
        return Ok(false);
    }
    let exists: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM receipts
            WHERE change_id = ?1 AND head = ?2
              AND submitter = ?3 AND signature != ''
        )",
        params![change, head, PREFLIGHT_SUBMITTER],
        |row| row.get(0),
    )?;
    Ok(exists)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Discovers Cargo workspace packages (name, relative directory, edition) from manifests
/// on disk without spawning `cargo metadata`.
pub fn discover_workspace_crates(worktree: &Path) -> (Vec<WorkspaceCrate>, String) {
    let Some(root_document) = read_manifest(worktree) else {
        let tauri = worktree.join("src-tauri");
        if let Some(document) = read_manifest(&tauri)
            && let Some(item) = package_from_manifest(&document, "src-tauri", "2021")
        {
            let edition = item.edition.clone();
            return (vec![item], edition);
        }
        return (Vec::new(), String::from("2024"));
    };
    let workspace_edition = root_document
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("edition"))
        .and_then(Item::as_str)
        .unwrap_or("2021")
        .to_owned();
    let mut crates = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(root_package) = package_from_manifest(&root_document, "", &workspace_edition) {
        seen.insert(String::new());
        crates.push(root_package);
    }
    let excludes = string_array(&root_document, "exclude");
    for member in string_array(&root_document, "members") {
        for folder in expand_member(worktree, &member) {
            let relative = relative_directory(worktree, &folder);
            if excludes
                .iter()
                .any(|pattern| matches_member(&relative, pattern))
            {
                continue;
            }
            if !seen.insert(relative.clone()) {
                continue;
            }
            if let Some(document) = read_manifest(&folder)
                && let Some(item) = package_from_manifest(&document, &relative, &workspace_edition)
            {
                crates.push(item);
            }
        }
    }
    (crates, workspace_edition)
}

fn read_manifest(folder: &Path) -> Option<DocumentMut> {
    let text = std::fs::read_to_string(folder.join("Cargo.toml")).ok()?;
    text.parse::<DocumentMut>().ok()
}

fn package_from_manifest(
    document: &DocumentMut,
    directory: &str,
    workspace_edition: &str,
) -> Option<WorkspaceCrate> {
    let package = document.get("package")?.as_table_like()?;
    let name = package.get("name")?.as_str()?.to_owned();
    let edition = match package.get("edition") {
        Some(item) if item.as_str().is_some() => {
            item.as_str().unwrap_or(workspace_edition).to_owned()
        }
        Some(item)
            if item
                .as_table_like()
                .and_then(|table| table.get("workspace"))
                .and_then(Item::as_bool)
                == Some(true) =>
        {
            workspace_edition.to_owned()
        }
        _ => String::from("2015"),
    };
    Some(WorkspaceCrate {
        name,
        directory: directory.to_owned(),
        edition,
    })
}

fn string_array(document: &DocumentMut, key: &str) -> Vec<String> {
    document
        .get("workspace")
        .and_then(|workspace| workspace.get(key))
        .and_then(Item::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(Value::as_str)
                .map(normalize_path)
                .collect()
        })
        .unwrap_or_default()
}

fn expand_member(worktree: &Path, member: &str) -> Vec<PathBuf> {
    if !member.contains(['*', '?', '[']) {
        let folder = worktree.join(member);
        return folder
            .join("Cargo.toml")
            .is_file()
            .then_some(folder)
            .into_iter()
            .collect();
    }
    let base = glob_prefix_folder(worktree, member);
    let mut found = Vec::new();
    scan_manifests(&base, MAX_GLOB_SCAN_DEPTH, &mut found);
    found
        .into_iter()
        .filter(|folder| matches_member(&relative_directory(worktree, folder), member))
        .collect()
}

fn glob_prefix_folder(root: &Path, pattern: &str) -> PathBuf {
    let mut prefix = root.to_path_buf();
    for component in Path::new(pattern).components() {
        let Component::Normal(segment) = component else {
            continue;
        };
        if segment.to_string_lossy().contains(['*', '?', '[']) {
            break;
        }
        prefix.push(segment);
    }
    prefix
}

fn scan_manifests(base: &Path, max_depth: usize, found: &mut Vec<PathBuf>) {
    let mut pending = vec![(base.to_path_buf(), 0usize)];
    while let Some((folder, depth)) = pending.pop() {
        if folder.join("Cargo.toml").is_file() {
            found.push(folder.clone());
        }
        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let skipped = entry
                .file_name()
                .to_str()
                .is_some_and(|name| SKIPPED_SCAN_FOLDERS.contains(&name));
            if !skipped {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
}

fn relative_directory(worktree: &Path, folder: &Path) -> String {
    folder
        .strip_prefix(worktree)
        .ok()
        .map(|relative| normalize_path(&relative.to_string_lossy()))
        .unwrap_or_default()
}

fn matches_member(path: &str, pattern: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    let path_parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let pattern_parts: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    match_segments(&pattern_parts, &path_parts)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            match_segments(&pattern[1..], path)
                || (!path.is_empty() && match_segments(pattern, &path[1..]))
        }
        (Some(head), Some(segment)) => {
            match_wildcards(head, segment) && match_segments(&pattern[1..], &path[1..])
        }
        _ => false,
    }
}

fn match_wildcards(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star = None;
    let mut match_ti = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            match_ti = ti;
            pi += 1;
        } else if let Some(star_index) = star {
            pi = star_index + 1;
            match_ti += 1;
            ti = match_ti;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|character| *character == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#560): preflight records must survive atomic save/load outside the workspace so
    // name and land gates reliably see both passing snapshots and failure summaries.
    #[test]
    fn preflight_record_survives_save_and_load() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let record = PreflightRecord {
            change: "qlxovkwwwxkppttrzxzyxmkulowyknot".into(),
            head: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            base: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            passed: false,
            failure_summary: "- [FAIL] cargo clippy -p velocity-core --all-targets — failed".into(),
            blocking_overlaps: Vec::new(),
            non_overlap_checks_passed: false,
            log_path: "C:/temp/preflight.log".into(),
            recorded_at: 42,
        };
        save_record(Some(dir.path()), &record)?;
        let loaded = load_record(Some(dir.path()), &record.change);
        assert_eq!(loaded, Some(record));
        Ok(())
    }

    // Risk (#560): only signed receipts submitted by `velocity-preflight` for the exact
    // current head may satisfy the Rosen receipt check.
    #[test]
    fn receipt_lookup_matches_signed_preflight_receipt_for_exact_head()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("metadata.sqlite");
        let conn = SqliteConnection::open(&db)?;
        conn.execute_batch(
            "CREATE TABLE receipts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                change_id TEXT NOT NULL,
                head TEXT NOT NULL,
                base TEXT NOT NULL,
                check_set TEXT NOT NULL,
                submitter TEXT NOT NULL,
                machine TEXT NOT NULL DEFAULT '',
                signature TEXT NOT NULL DEFAULT ''
            );",
        )?;
        let change = "qlxovkwwwxkppttrzxzyxmkulowyknot";
        let head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let other_head = "cccccccccccccccccccccccccccccccccccccccc";
        assert!(!has_preflight_receipt(dir.path(), change, head)?);
        // Unsigned rows or rows from another submitter must not count.
        conn.execute(
            "INSERT INTO receipts (change_id, head, base, check_set, submitter, machine, signature)
             VALUES (?1, ?2, 'base', 'pre-land', ?3, 'server', '')",
            params![change, head, PREFLIGHT_SUBMITTER],
        )?;
        conn.execute(
            "INSERT INTO receipts (change_id, head, base, check_set, submitter, machine, signature)
             VALUES (?1, ?2, 'base', 'pre-land', 'someone-else', 'server', 'deadbeef')",
            params![change, head],
        )?;
        assert!(!has_preflight_receipt(dir.path(), change, head)?);

        conn.execute(
            "INSERT INTO receipts (change_id, head, base, check_set, submitter, machine, signature)
             VALUES (?1, ?2, 'base', 'pre-land', ?3, 'server', 'deadbeef')",
            params![change, head, PREFLIGHT_SUBMITTER],
        )?;
        assert!(has_preflight_receipt(dir.path(), change, head)?);
        assert!(!has_preflight_receipt(dir.path(), change, other_head)?);
        Ok(())
    }
}
