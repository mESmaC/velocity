//! Integration scheduling reads Rosen's land ledger. Exact passes and the GitHub
//! outbox are kept in a separate Velocity-owned database.

use std::fs::{File, OpenOptions};
use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::{ProjectServer, RosenError};

pub const FAILURE_HEADING: &str = "Integration build failed\n";
pub const FULL_SUITE: &str = "node scripts/check-full.mjs --integration";
const LAND_LIMIT: u32 = 10;
const AGE_LIMIT: Duration = Duration::from_hours(2);

pub fn try_slot() -> io::Result<Option<File>> {
    let slot = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(std::env::temp_dir().join("velocity-integration-build.lock"))?;
    match slot.try_lock() {
        Ok(()) => Ok(Some(slot)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

pub fn due(server: &ProjectServer, commit: &str, idle: bool) -> Result<bool, RosenError> {
    let checkpoint: Option<(String, bool)> = progress(server)?
        .query_row(
            "SELECT commit_id, passed FROM attempts ORDER BY rowid DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((last, passed)) = &checkpoint
        && last == commit
    {
        // Re-send a successful result if Velocity stopped before Rosen recorded it.
        return Ok(*passed);
    }
    if idle {
        return Ok(true);
    }
    let last = checkpoint.as_ref().map(|(last, _)| last.as_str());
    let (count, started): (u32, Option<u64>) = ledger(server)?.query_row(
        "WITH target AS (SELECT ref_name,seq FROM base_history WHERE commit_id=?1),
         pending AS (
             SELECT h.commit_id FROM base_history h JOIN target t ON h.ref_name=t.ref_name
             WHERE h.change_id IS NOT NULL AND h.seq<=t.seq AND h.seq>COALESCE(
                 (SELECT seq FROM base_history WHERE ref_name=t.ref_name AND commit_id=?2),
                 (SELECT seq FROM post_land_green WHERE ref_name=t.ref_name), 0))
         SELECT (SELECT COUNT(*) FROM pending),
                (SELECT MIN(created_at) FROM events WHERE kind IN ('landed','reverted')
                 AND json_extract(detail,'$.commit') IN (SELECT commit_id FROM pending))",
        params![commit, last],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Ok(count >= LAND_LIMIT
        || started
            .is_some_and(|started| now.saturating_sub(Duration::from_millis(started)) >= AGE_LIMIT))
}

pub fn result(server: &ProjectServer, commit: &str, passed: bool) -> Result<(), RosenError> {
    progress(server)?.execute(
        "INSERT INTO attempts(commit_id,passed) VALUES (?1,?2)
         ON CONFLICT(commit_id) DO UPDATE SET passed=excluded.passed",
        params![commit, passed],
    )?;
    Ok(())
}

pub fn passed(server: &ProjectServer, commit: &str) -> Result<bool, RosenError> {
    Ok(progress(server)?
        .query_row(
            "SELECT passed FROM attempts WHERE commit_id=?1",
            [commit],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

pub fn last_green(server: &ProjectServer) -> Result<Option<String>, RosenError> {
    Ok(progress(server)?
        .query_row(
            "SELECT commit_id FROM attempts WHERE passed ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

/// Saves why an integration build failed, for the stuck-integration notice (#504).
pub fn record_failure(
    server: &ProjectServer,
    commit: &str,
    reason: &str,
) -> Result<(), RosenError> {
    progress(server)?.execute(
        "INSERT INTO failures(commit_id,reason) VALUES (?1,?2)
         ON CONFLICT(commit_id) DO UPDATE SET reason=excluded.reason",
        params![commit, reason],
    )?;
    Ok(())
}

/// One line on the last finished integration build: why it failed, or which base passed.
/// `None` when no integration build has finished.
pub fn last_outcome(server: &ProjectServer) -> Result<Option<String>, RosenError> {
    let last: Option<(String, bool, Option<String>)> = progress(server)?
        .query_row(
            "SELECT a.commit_id, a.passed, f.reason FROM attempts a
             LEFT JOIN failures f ON f.commit_id=a.commit_id ORDER BY a.rowid DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(last.map(|(commit, passed, reason)| {
        let short = commit.get(..7).unwrap_or(&commit);
        if passed {
            format!("passed at {short} before these lands")
        } else {
            let reason = reason.unwrap_or_else(|| "no reason was recorded".into());
            format!("failed at {short}: {reason}")
        }
    }))
}

pub fn unpublished(server: &ProjectServer) -> Result<Vec<String>, RosenError> {
    let database = progress(server)?;
    let mut query = database.prepare(
        "SELECT commit_id FROM attempts WHERE passed AND NOT published ORDER BY rowid DESC LIMIT 4",
    )?;
    let rows = query.query_map([], |row| row.get(0))?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

pub fn published(server: &ProjectServer, commit: &str) -> Result<(), RosenError> {
    progress(server)?.execute(
        "UPDATE attempts SET published=1 WHERE commit_id=?1",
        [commit],
    )?;
    Ok(())
}

fn progress(server: &ProjectServer) -> Result<Connection, RosenError> {
    let connection = Connection::open(server.data_dir().join("integration.sqlite"))?;
    connection.busy_timeout(Duration::from_secs(2))?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS attempts (
             commit_id TEXT NOT NULL UNIQUE, passed INTEGER NOT NULL,
             published INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS failures (
             commit_id TEXT NOT NULL UNIQUE, reason TEXT NOT NULL
         )",
    )?;
    Ok(connection)
}

fn ledger(server: &ProjectServer) -> Result<Connection, RosenError> {
    let connection = Connection::open_with_flags(
        server.data_dir().join("metadata.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    connection.busy_timeout(Duration::from_secs(2))?;
    Ok(connection)
}

/// The test offer Rosen's public RPC defers while a merge-mode build is pending.
/// The server still validates the resulting job and owns bisection and delivery.
pub(crate) async fn merge_test_job(
    server: &ProjectServer,
    client: &rosen_client::Client,
) -> Result<Option<rosen_client::protocol::PostLandJob>, RosenError> {
    use rosen_client::protocol::{CheckKind, CheckPriority, PostLandJob};
    let commit: Option<String> = ledger(server)?
        .query_row(
            "SELECT h.commit_id FROM base_history h JOIN base_refs r ON r.name=h.ref_name
         WHERE h.change_id IS NOT NULL AND h.seq>COALESCE(
             (SELECT seq FROM post_land_green WHERE ref_name=h.ref_name),0)
           AND h.seq>COALESCE((SELECT MAX(failed.seq) FROM post_land_failures f
             JOIN bisections b ON b.kind=f.kind AND b.phase='done'
                 AND json_extract(b.commits,'$[' || b.high || '][0]')=f.commit_id
             JOIN base_history failed ON failed.ref_name=b.ref_name AND failed.commit_id=b.failed
             WHERE b.ref_name=h.ref_name AND f.kind='test' AND f.requires_revert=0),0)
         ORDER BY h.seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(commit) = commit else {
        return Ok(None);
    };
    let commit = crate::policy::object(&commit)?;
    let config = client.project_config(Some(commit)).await?.config;
    Ok(Some(PostLandJob {
        commit,
        kind: CheckKind::Test,
        priority: CheckPriority::Verify,
        check_set: config.checks.post_land,
        // An offered job carries no submitter; Rosen records it with the result.
        submitter: None,
    }))
}
