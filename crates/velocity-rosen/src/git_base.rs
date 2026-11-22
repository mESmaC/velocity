//! Read Rosen's current base commit from its local metadata (used during startup migration).

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::{ProjectServer, RosenError};

pub(crate) fn read_base(server: &ProjectServer) -> Result<Option<String>, RosenError> {
    let metadata = server.data_dir().join("metadata.sqlite");
    if !metadata.is_file() {
        return Ok(None);
    }
    let connection =
        Connection::open_with_flags(metadata, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(failure)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(2))
        .map_err(failure)?;
    connection
        .query_row("SELECT target FROM base_refs LIMIT 1", [], |row| row.get(0))
        .optional()
        .map_err(failure)
}

fn failure(error: impl std::fmt::Display) -> RosenError {
    RosenError::GitBase(error.to_string())
}
