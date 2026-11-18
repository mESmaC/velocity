use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use super::{BaseStatus, LandingStatus, MirrorStatus};
use crate::{ProjectServer, RosenError};

pub(super) struct Metadata {
    pub base: BaseStatus,
    pub landing: LandingStatus,
    pub mirror: MirrorStatus,
    pub machines: Vec<String>,
    pub changes: BTreeMap<String, ChangeHistory>,
    pub conflicts: BTreeMap<String, Vec<String>>,
    pub expiry: Duration,
    pub live: Vec<(String, String)>,
}

#[derive(Default)]
pub(super) struct ChangeHistory {
    pub landed: Option<String>,
    pub reverted: bool,
    pub green: bool,
    pub output: Option<String>,
}

const HOUR: i64 = 3_600_000;
const PRESENCE: i64 = 90_000;

// R9 carries lifecycle notifications but omits event times and several read-only
// fields. Read the pinned daemon's metadata together, without writing or migrating it.
pub(super) fn read(server: &ProjectServer) -> Result<Metadata, RosenError> {
    let mut connection = Connection::open_with_flags(
        server.data_dir().join("metadata.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(data_error)?;
    connection
        .busy_timeout(Duration::from_millis(200))
        .map_err(data_error)?;
    let transaction = connection.transaction().map_err(data_error)?;
    snapshot(&transaction).map_err(data_error)
}

fn data_error(error: rusqlite::Error) -> RosenError {
    RosenError::StatusData(error.to_string())
}

fn snapshot(connection: &Connection) -> rusqlite::Result<Metadata> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let now = i64::try_from(now.as_millis()).unwrap_or(i64::MAX);
    let (base, reference, green_sequence) = base(connection)?;
    let (mut landing, next_land) = landing(connection, now)?;
    landing.builds_pending = backlog(connection, &reference, true)?;
    landing.tests_pending = backlog(connection, &reference, false)?;
    let (machines, next_machine) = machines(connection, now)?;
    let expiry = [next_land, next_machine]
        .into_iter()
        .flatten()
        .min()
        .map(|deadline| Duration::from_millis(deadline.saturating_sub(now).max(1) as u64))
        .unwrap_or(Duration::from_secs(86_400));
    let mirror = mirror(connection, base.tip.as_deref())?;
    Ok(Metadata {
        base,
        landing,
        mirror,
        machines,
        changes: changes(connection, green_sequence)?,
        conflicts: conflicts(connection)?,
        expiry,
        live: live(connection)?,
    })
}

fn table(connection: &Connection, name: &str) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )
}

fn base(connection: &Connection) -> rusqlite::Result<(BaseStatus, String, i64)> {
    let mut base = BaseStatus::default();
    let (reference, tip): (String, Option<String>) = if table(connection, "base_refs")? {
        connection
            .query_row("SELECT name, target FROM base_refs LIMIT 1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?
            .unwrap_or_default()
    } else {
        Default::default()
    };
    base.tip = tip;
    let mut green_sequence = 0;
    if table(connection, "post_land_green")? {
        (green_sequence, base.green) = connection
            .query_row(
                "SELECT seq, commit_id FROM post_land_green WHERE ref_name = ?1",
                [&reference],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or_default();
    }
    if table(connection, "bisections")? {
        base.bisecting = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM bisections WHERE ref_name = ?1
               AND phase IN ('search', 'confirm', 'parent'))",
            [&reference],
            |row| row.get(0),
        )?;
    }
    (base.culprit, base.revert) =
        current_failure(connection, &reference, base.tip.as_deref(), green_sequence)?;
    Ok((base, reference, green_sequence))
}

fn current_failure(
    connection: &Connection,
    reference: &str,
    tip: Option<&str>,
    test_green: i64,
) -> rusqlite::Result<(Option<String>, Option<String>)> {
    if !table(connection, "post_land_failures")? || !table(connection, "base_history")? {
        return Ok((None, None));
    }
    let build_green = green_prefix(connection, reference, "post_land_build_green")?;
    Ok(connection
        .query_row(
            "SELECT failure.commit_id, failure.revert
               FROM post_land_failures failure
               JOIN base_history history ON history.commit_id = failure.commit_id
              WHERE history.ref_name = ?1
                AND (failure.revert = ?2 OR (failure.revert IS NULL
                     AND history.seq > CASE failure.kind WHEN 'build' THEN ?3 ELSE ?4 END))
              ORDER BY failure.session DESC LIMIT 1",
            (reference, tip, build_green, test_green),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or_default())
}

fn green_prefix(
    connection: &Connection,
    reference: &str,
    table_name: &str,
) -> rusqlite::Result<i64> {
    if !table(connection, table_name)? {
        return Ok(0);
    }
    Ok(connection
        .query_row(
            &format!("SELECT seq FROM {table_name} WHERE ref_name = ?1"),
            [reference],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default())
}

fn landing(connection: &Connection, now: i64) -> rusqlite::Result<(LandingStatus, Option<i64>)> {
    let mut status = LandingStatus::default();
    if !table(connection, "events")? {
        return Ok((status, None));
    }
    let mut statement = connection.prepare(
        "SELECT land.created_at, (
             SELECT land.created_at - name.created_at FROM events name
             WHERE name.change_id = land.change_id AND name.kind = 'snapshot'
               AND json_extract(name.detail, '$.named') = 1 AND name.seq < land.seq
               AND name.seq > COALESCE((SELECT MAX(previous.seq) FROM events previous
                    WHERE previous.change_id = land.change_id AND previous.kind = 'landed'
                      AND previous.seq < land.seq), 0)
             ORDER BY name.seq DESC LIMIT 1
         ) FROM events land WHERE land.kind = 'landed' AND land.created_at > ?1",
    )?;
    let mut ages = Vec::new();
    let mut next = None;
    for row in statement.query_map([now.saturating_sub(HOUR)], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
    })? {
        let (at, age) = row?;
        let expiry = at.saturating_add(HOUR);
        next = Some(next.map_or(expiry, |current: i64| current.min(expiry)));
        status.lands_last_hour += 1;
        if let Some(age) = age.and_then(|age| u32::try_from(age).ok()) {
            ages.push(age);
        }
    }
    ages.sort_unstable();
    if !ages.is_empty() {
        status.median_ms = Some(ages[(ages.len() - 1) / 2].midpoint(ages[ages.len() / 2]));
    }
    Ok((status, next))
}

fn backlog(connection: &Connection, reference: &str, build: bool) -> rusqlite::Result<u32> {
    if !table(connection, "base_history")? {
        return Ok(0);
    }
    let progress = if build {
        "post_land_build_green"
    } else {
        "post_land_green"
    };
    let mut after = green_prefix(connection, reference, progress)?;
    if table(connection, "bisections")? {
        let (kind, phases) = if build {
            ("build", "('done','known')")
        } else {
            ("test", "('done')")
        };
        after = after.max(connection.query_row(
            &format!(
                "SELECT COALESCE(MAX(h.seq), 0) FROM bisections b
              JOIN base_history h ON h.ref_name = b.ref_name AND h.commit_id = b.failed
              WHERE b.ref_name = ?1 AND b.kind = ?2 AND b.phase IN {phases}"
            ),
            [reference, kind],
            |row| row.get(0),
        )?);
    }
    let land = if build {
        "tier != 'initial'"
    } else {
        "change_id IS NOT NULL"
    };
    connection.query_row(
        &format!("SELECT COUNT(*) FROM base_history WHERE ref_name = ?1 AND seq > ?2 AND {land}"),
        (reference, after),
        |row| row.get(0),
    )
}

fn machines(connection: &Connection, now: i64) -> rusqlite::Result<(Vec<String>, Option<i64>)> {
    if !table(connection, "machines")? {
        return Ok((Vec::new(), None));
    }
    let mut statement = connection.prepare(
        "SELECT name, MIN(last_contact * 1000 + ?3, expires_at * 1000) FROM machines WHERE status = 'active'
           AND (last_contact * 1000) > ?1 AND (expires_at * 1000) > ?2 ORDER BY name",
    )?;
    let mut names = Vec::new();
    let mut next = None;
    for row in statement.query_map([now.saturating_sub(PRESENCE), now, PRESENCE], |row| {
        Ok((row.get(0)?, row.get::<_, i64>(1)?))
    })? {
        let (name, expiry) = row?;
        names.push(name);
        next = Some(next.map_or(expiry, |current: i64| current.min(expiry)));
    }
    Ok((names, next))
}

fn mirror(connection: &Connection, tip: Option<&str>) -> rusqlite::Result<MirrorStatus> {
    let mut status = MirrorStatus::default();
    if !table(connection, "mirror")? {
        return Ok(status);
    }
    (status.remote, status.pushed) = connection
        .query_row(
            "SELECT remote, pushed FROM mirror WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or_default();
    let Some(remote) = &status.remote else {
        return Ok(status);
    };
    status.collapse = remote.starts_with("https://collapse.sh/")
        || remote.starts_with("https://git.collapse.sh/");
    let mut label = "Pending";
    if status
        .pushed
        .as_deref()
        .is_some_and(|pushed| Some(pushed) == tip)
    {
        label = "Synced";
    } else if table(connection, "events")?
        && let Some((failed, rejected, remote, error)) = connection
            .query_row(
                "SELECT json_extract(detail, '$.status') = 'failed',
             COALESCE(json_extract(detail, '$.rejected'), 0),
             json_extract(detail, '$.remote'), json_extract(detail, '$.error')
             FROM events WHERE kind = 'mirror' ORDER BY seq DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get(3)?,
                    ))
                },
            )
            .optional()?
        && failed
        && remote == status.remote
    {
        label = if rejected { "Rejected" } else { "Retrying" };
        status.error = error;
    }
    status.state = Some(label.into());
    Ok(status)
}

fn changes(
    connection: &Connection,
    green: i64,
) -> rusqlite::Result<BTreeMap<String, ChangeHistory>> {
    let mut changes = BTreeMap::new();
    if table(connection, "base_history")? {
        let mut statement = connection.prepare(
            "SELECT change_id, commit_id, seq FROM base_history WHERE change_id IS NOT NULL
             ORDER BY seq",
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)?))
        })? {
            let (change, commit, seq) = row?;
            changes.insert(
                change,
                ChangeHistory {
                    landed: Some(commit),
                    green: seq <= green,
                    ..Default::default()
                },
            );
        }
    }
    if table(connection, "events")? {
        read_reverts(connection, &mut changes)?;
    }
    Ok(changes)
}

fn read_reverts(
    connection: &Connection,
    changes: &mut BTreeMap<String, ChangeHistory>,
) -> rusqlite::Result<()> {
    let mut statement = connection.prepare(
        "SELECT e.change_id, json_extract(e.detail, '$.output'),
          e.seq > COALESCE((SELECT MAX(n.seq) FROM events n WHERE n.change_id = e.change_id
                      AND n.kind = 'snapshot' AND json_extract(n.detail, '$.named') = 1), 0)
         FROM events e WHERE e.kind = 'reverted'
           AND e.change_id IS NOT NULL AND NOT EXISTS (
               SELECT 1 FROM events l WHERE l.change_id = e.change_id AND l.kind = 'landed' AND l.seq > e.seq
           )
         ORDER BY e.seq",
    )?;
    for row in statement.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get::<_, bool>(2)?))
    })? {
        let (change, output, reopened) = row?;
        let record = changes.entry(change).or_default();
        record.output = output;
        record.reverted = reopened;
    }
    Ok(())
}

fn conflicts(connection: &Connection) -> rusqlite::Result<BTreeMap<String, Vec<String>>> {
    let mut conflicts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if table(connection, "conflicts")? {
        let mut statement = connection.prepare(
            "SELECT DISTINCT change_id, path FROM conflicts WHERE state = 'open' ORDER BY change_id, path",
        )?;
        for row in statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))? {
            let (change, path) = row?;
            conflicts.entry(change).or_default().push(path);
        }
    }
    Ok(conflicts)
}

fn live(connection: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    if !table(connection, "changes")? {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare("SELECT id, state FROM changes WHERE state IN ('open', 'ready') ORDER BY id")?;
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}
