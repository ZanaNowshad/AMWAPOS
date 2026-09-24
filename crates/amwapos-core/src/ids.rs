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

/// Standard base64 (files handed to the UI for download or preview).
pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let s = s.split_once("base64,").map(|x| x.1).unwrap_or(s);
    base64::engine::general_purpose::STANDARD.decode(s.trim()).ok()
}
