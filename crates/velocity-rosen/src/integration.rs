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
    if let Some((last, passed)) = &checkpoint {
        if last == commit {
            // Re-send a successful result if Velocity stopped before Rosen recorded it.
            return Ok(*passed);
        }
        // The base moved past a failed batch (for example a fix-forward landed):
        // verify the new tip immediately so it goes green or names a fresh culprit (#567).
        if !*passed {
            return Ok(true);
        }
    }
    if idle {
        return Ok(true);
    }
    let metadata = ledger(server)?;
    if has_uncovered_failure_before(&metadata, commit)? {
        return Ok(true);
    }
    let last = checkpoint.as_ref().map(|(last, _)| last.as_str());
    let (count, started) = pending_window(&metadata, commit, last)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Ok(count >= LAND_LIMIT
        || started
            .is_some_and(|started| now.saturating_sub(Duration::from_millis(started)) >= AGE_LIMIT))
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, RosenError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )?)
}

fn green_seq_for_ref(connection: &Connection, reference: &str) -> Result<i64, RosenError> {
    if !table_exists(connection, "post_land_green")? {
        return Ok(0);
    }
    Ok(connection
        .query_row(
            "SELECT seq FROM post_land_green WHERE ref_name=?1",
            [reference],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

fn has_uncovered_failure_before(connection: &Connection, commit: &str) -> Result<bool, RosenError> {
    if !table_exists(connection, "base_history")?
        || !table_exists(connection, "post_land_failures")?
    {
        return Ok(false);
    }
    let Some((reference, target_seq)): Option<(String, i64)> = connection
        .query_row(
            "SELECT ref_name, seq FROM base_history WHERE commit_id=?1 LIMIT 1",
            [commit],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let green_seq = green_seq_for_ref(connection, &reference)?;
    Ok(connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM post_land_failures f
             JOIN base_history h ON h.ref_name=?1 AND h.commit_id=f.commit_id
             WHERE f.revert IS NULL AND h.seq>?2 AND h.seq<?3
         )",
        params![reference, green_seq, target_seq],
        |row| row.get(0),
    )?)
}

fn pending_window(
    connection: &Connection,
    commit: &str,
    last: Option<&str>,
) -> Result<(u32, Option<u64>), RosenError> {
    if !table_exists(connection, "base_history")? {
        return Ok((0, None));
    }
    let Some((reference, target_seq)): Option<(String, i64)> = connection
        .query_row(
            "SELECT ref_name, seq FROM base_history WHERE commit_id=?1 LIMIT 1",
            [commit],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
    else {
        return Ok((0, None));
    };
    let last_seq: Option<i64> = match last {
        Some(commit) => connection
            .query_row(
                "SELECT seq FROM base_history WHERE ref_name=?1 AND commit_id=?2",
                params![reference, commit],
                |row| row.get(0),
            )
            .optional()?,
        None => None,
    };
    let floor = match last_seq {
        Some(seq) => seq,
        None => green_seq_for_ref(connection, &reference)?,
    };
    let count: u32 = connection.query_row(
        "SELECT COUNT(*) FROM base_history
         WHERE ref_name=?1 AND change_id IS NOT NULL AND seq<=?2 AND seq>?3",
        params![reference, target_seq, floor],
        |row| row.get(0),
    )?;
    if count == 0 || !table_exists(connection, "events")? {
        return Ok((count, None));
    }
    let started: Option<u64> = connection.query_row(
        "SELECT MIN(created_at) FROM events
         WHERE kind IN ('landed','reverted') AND json_extract(detail,'$.commit') IN (
             SELECT commit_id FROM base_history
             WHERE ref_name=?1 AND change_id IS NOT NULL AND seq<=?2 AND seq>?3
         )",
        params![reference, target_seq, floor],
        |row| row.get(0),
    )?;
    Ok((count, started))
}

pub fn result(server: &ProjectServer, commit: &str, passed: bool) -> Result<(), RosenError> {
    let mut connection = progress(server)?;
    let transaction = connection.transaction()?;
    let existing: Option<(bool, bool)> = transaction
        .query_row(
            "SELECT passed, published FROM attempts WHERE commit_id=?1",
            [commit],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let published =
        existing.is_some_and(|(was_passed, was_published)| passed && was_passed && was_published);
    // Replace the row so `ORDER BY rowid DESC LIMIT 1` always observes the latest attempt.
    transaction.execute("DELETE FROM attempts WHERE commit_id=?1", [commit])?;
    transaction.execute(
        "INSERT INTO attempts(commit_id,passed,published) VALUES (?1,?2,?3)",
        params![commit, passed, published],
    )?;
    transaction.commit()?;
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

/// Whether `commit` already has a recorded failing integration attempt (#567).
pub fn failed(server: &ProjectServer, commit: &str) -> Result<bool, RosenError> {
    Ok(progress(server)?
        .query_row(
            "SELECT NOT passed FROM attempts WHERE commit_id=?1",
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

/// The lands on the base that no green build covers yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Uncovered {
    pub lands: u32,
    /// When the oldest of them landed, in Unix seconds.
    pub since: Option<u64>,
}

/// What the base holds past its last green build, for the uncovered-base notice (#503).
/// The last green build is the later of the last integration build that passed and
/// Rosen's own post-land checkpoint, which also advances for a project with no commands.
pub fn uncovered(server: &ProjectServer) -> Result<Uncovered, RosenError> {
    let connection = ledger(server)?;
    let Some(reference) = base_ref_name(&connection)? else {
        return Ok(Uncovered::default());
    };
    let green = last_green(server)?;
    let integration_seq: i64 = match green.as_deref() {
        Some(commit) => connection
            .query_row(
                "SELECT seq FROM base_history WHERE ref_name=?1 AND commit_id=?2",
                params![reference, commit],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0),
        None => 0,
    };
    let floor = integration_seq.max(green_seq_for_ref(&connection, &reference)?);
    let (lands, since) = pending_since_floor(&connection, &reference, floor)?;
    Ok(Uncovered {
        lands,
        since: since.map(|milliseconds| milliseconds / 1000),
    })
}

/// Landed changes past Rosen's `post_land_green` that have had no post-land result (#567).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostLandStall {
    pub lands: u32,
    /// The oldest landed commit not yet covered by Rosen's `post_land_green`.
    pub oldest_commit: String,
    /// When the stall started (Unix seconds): the later of the oldest unverified land
    /// and the latest post-land result event.
    pub since: u64,
}

/// Reads landed commits newer than Rosen's `post_land_green` and when post-land verification
/// last produced any result, for the project health stall check (#567).
pub fn post_land_stall(server: &ProjectServer) -> Result<Option<PostLandStall>, RosenError> {
    let connection = ledger(server)?;
    let Some(reference) = base_ref_name(&connection)? else {
        return Ok(None);
    };
    let floor = green_seq_for_ref(&connection, &reference)?;
    let oldest_commit: Option<String> = connection
        .query_row(
            "SELECT commit_id FROM base_history
             WHERE ref_name=?1 AND change_id IS NOT NULL AND seq>?2
             ORDER BY seq ASC LIMIT 1",
            params![reference, floor],
            |row| row.get(0),
        )
        .optional()?;
    let Some(oldest_commit) = oldest_commit else {
        return Ok(None);
    };
    let (lands, oldest_landed_ms) = pending_since_floor(&connection, &reference, floor)?;
    if lands == 0 {
        return Ok(None);
    }
    let last_result_ms: Option<u64> = if table_exists(&connection, "events")? {
        connection.query_row(
            "SELECT MAX(created_at) FROM events
             WHERE kind IN ('green','built','post_land_failure','bisect')",
            [],
            |row| row.get(0),
        )?
    } else {
        None
    };
    let Some(since_ms) = oldest_landed_ms.max(last_result_ms) else {
        return Ok(None);
    };
    Ok(Some(PostLandStall {
        lands,
        oldest_commit,
        since: since_ms / 1000,
    }))
}

fn base_ref_name(connection: &Connection) -> Result<Option<String>, RosenError> {
    if !table_exists(connection, "base_refs")? || !table_exists(connection, "base_history")? {
        return Ok(None);
    }
    Ok(connection
        .query_row("SELECT name FROM base_refs LIMIT 1", [], |row| row.get(0))
        .optional()?)
}

fn pending_since_floor(
    connection: &Connection,
    reference: &str,
    floor: i64,
) -> Result<(u32, Option<u64>), RosenError> {
    let lands: u32 = connection.query_row(
        "SELECT COUNT(*) FROM base_history
         WHERE ref_name=?1 AND change_id IS NOT NULL AND seq>?2",
        params![reference, floor],
        |row| row.get(0),
    )?;
    if lands == 0 || !table_exists(connection, "events")? {
        return Ok((lands, None));
    }
    let since: Option<u64> = connection.query_row(
        "SELECT MIN(created_at) FROM events
         WHERE kind IN ('landed','reverted') AND json_extract(detail,'$.commit') IN (
             SELECT commit_id FROM base_history
             WHERE ref_name=?1 AND change_id IS NOT NULL AND seq>?2
         )",
        params![reference, floor],
        |row| row.get(0),
    )?;
    Ok((lands, since))
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
    let connection = ledger(server)?;
    let Some(reference) = base_ref_name(&connection)? else {
        return Ok(None);
    };
    let green_seq = green_seq_for_ref(&connection, &reference)?;
    let failed_seq = last_non_reverting_failed_seq(&connection, &reference)?;
    let floor = green_seq.max(failed_seq);
    let commit: Option<String> = connection
        .query_row(
            "SELECT commit_id FROM base_history
             WHERE ref_name=?1 AND change_id IS NOT NULL AND seq>?2
             ORDER BY seq DESC LIMIT 1",
            params![reference, floor],
            |row| row.get(0),
        )
        .optional()?;
    build_test_job(client, commit).await
}

/// The newest landed commit past Rosen's `post_land_green`, ignoring prior failed-batch
/// suppression so startup resume or a forced integration can re-verify the tip (#567).
pub(crate) async fn tip_test_job(
    server: &ProjectServer,
    client: &rosen_client::Client,
) -> Result<Option<rosen_client::protocol::PostLandJob>, RosenError> {
    let connection = ledger(server)?;
    let Some(reference) = base_ref_name(&connection)? else {
        return Ok(None);
    };
    let floor = green_seq_for_ref(&connection, &reference)?;
    let commit: Option<String> = connection
        .query_row(
            "SELECT commit_id FROM base_history
             WHERE ref_name=?1 AND change_id IS NOT NULL AND seq>?2
             ORDER BY seq DESC LIMIT 1",
            params![reference, floor],
            |row| row.get(0),
        )
        .optional()?;
    build_test_job(client, commit).await
}

fn last_non_reverting_failed_seq(
    connection: &Connection,
    reference: &str,
) -> Result<i64, RosenError> {
    if !table_exists(connection, "post_land_failures")? || !table_exists(connection, "bisections")?
    {
        return Ok(0);
    }
    let seq: i64 = connection.query_row(
        "SELECT COALESCE(MAX(failed.seq), 0) FROM post_land_failures f
         JOIN bisections b ON b.kind=f.kind AND b.phase='done'
             AND json_extract(b.commits,'$[' || b.high || '][0]')=f.commit_id
         JOIN base_history failed ON failed.ref_name=b.ref_name AND failed.commit_id=b.failed
         WHERE b.ref_name=?1 AND f.kind='test' AND f.requires_revert=0",
        [reference],
        |row| row.get(0),
    )?;
    Ok(seq.max(0))
}

async fn build_test_job(
    client: &rosen_client::Client,
    commit: Option<String>,
) -> Result<Option<rosen_client::protocol::PostLandJob>, RosenError> {
    use rosen_client::protocol::{CheckKind, CheckPriority, PostLandJob};
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

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_server(
        name: &str,
    ) -> Result<(std::path::PathBuf, ProjectServer), Box<dyn std::error::Error>> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root = std::env::temp_dir().join(format!("velocity-567-{name}-{unique}"));
        let server = ProjectServer::new(&root, 1);
        std::fs::create_dir_all(server.data_dir())?;
        let conn = Connection::open(server.data_dir().join("metadata.sqlite"))?;
        conn.execute_batch(
            "CREATE TABLE base_refs (name TEXT PRIMARY KEY, target TEXT NOT NULL);
             CREATE TABLE base_history (
                 ref_name TEXT NOT NULL, seq INTEGER NOT NULL,
                 commit_id TEXT NOT NULL, change_id TEXT
             );
             CREATE TABLE post_land_green (ref_name TEXT PRIMARY KEY, seq INTEGER NOT NULL, commit_id TEXT NOT NULL);
             CREATE TABLE post_land_failures (
                 session INTEGER PRIMARY KEY, change_id TEXT NOT NULL, commit_id TEXT NOT NULL,
                 kind TEXT NOT NULL, output TEXT NOT NULL, requires_revert INTEGER NOT NULL, revert TEXT
             );
             CREATE TABLE events (seq INTEGER PRIMARY KEY, kind TEXT NOT NULL, detail TEXT NOT NULL, created_at INTEGER NOT NULL);
             INSERT INTO base_refs(name, target) VALUES ('master', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb');
             INSERT INTO post_land_green(ref_name, seq, commit_id) VALUES ('master', 1, '0000000000000000000000000000000000000000');
             INSERT INTO base_history(ref_name, seq, commit_id, change_id) VALUES
                 ('master', 1, '0000000000000000000000000000000000000000', 'c0'),
                 ('master', 2, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'c1'),
                 ('master', 3, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'c2');",
        )?;
        Ok((root, server))
    }

    // Risk (#567): after a failed batch, landing a fix-forward commit must immediately become
    // due for re-verification rather than waiting for 10 lands or 2 hours, while an unfixed
    // failing tip must not spin-loop.
    #[test]
    fn due_re_verifies_immediately_after_failed_batch_advances()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, server) = seed_server("due")?;
        let first = "a".repeat(40);
        let fix = "b".repeat(40);
        result(&server, &first, false)?;
        assert!(!due(&server, &first, false)?);
        assert!(due(&server, &fix, false)?);
        result(&server, &fix, true)?;
        assert!(due(&server, &fix, false)?);
        // Re-recording an older commit as failed updates its rowid to become the latest attempt.
        result(&server, &first, false)?;
        assert!(due(&server, &fix, false)?);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    // Risk (#567): landed changes past post_land_green with no post-land result must report
    // the count, oldest unverified commit, and stall start time, clearing once green advances.
    #[test]
    fn post_land_stall_tracks_oldest_unverified_commit_and_clears_on_green()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, server) = seed_server("stall")?;
        let first = "a".repeat(40);
        let fix = "b".repeat(40);
        let conn = Connection::open(server.data_dir().join("metadata.sqlite"))?;
        conn.execute(
            "INSERT INTO events(seq, kind, detail, created_at) VALUES
                (1, 'landed', ?1, 100000),
                (2, 'landed', ?2, 120000),
                (3, 'post_land_failure', '{}', 180000)",
            params![
                format!(r#"{{"commit":"{first}"}}"#),
                format!(r#"{{"commit":"{fix}"}}"#),
            ],
        )?;
        let stall = post_land_stall(&server)?.ok_or("expected active stall")?;
        assert_eq!(stall.lands, 2);
        assert_eq!(stall.oldest_commit, first);
        assert_eq!(stall.since, 180);
        conn.execute(
            "UPDATE post_land_green SET seq=3, commit_id=?1 WHERE ref_name='master'",
            [&fix],
        )?;
        // Close the SQLite handle before removing the temp directory on Windows.
        drop(conn);
        assert_eq!(post_land_stall(&server)?, None);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
