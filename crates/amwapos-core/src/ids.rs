//! Stable globally unique identifiers (ULID) and display-number sequences.

use rusqlite::{params, Connection};

use crate::error::AppResult;

pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

/// Atomically increment and return a named counter. Must be called inside the
/// caller's write transaction so the number is consumed only if it commits.
pub fn next_seq(conn: &Connection, name: &str) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO sequences(name, value) VALUES (?1, 1)
         ON CONFLICT(name) DO UPDATE SET value = value + 1",
        params![name],
    )?;
    Ok(conn.query_row("SELECT value FROM sequences WHERE name = ?1", params![name], |r| r.get(0))?)
}
