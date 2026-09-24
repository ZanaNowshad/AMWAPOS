//! Exactly-once operation model.
//!
//! The check, the business mutation and the completion record all run inside
//! ONE write transaction. Therefore an operation is either fully committed
//! (and recorded as completed) or not visible at all; there is no persisted
//! "in progress" state to resolve after a crash. A retry with the same
//! operation id and the same payload returns the original result; the same id
//! with a different payload is rejected as an integrity violation.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult, ErrorCode};
use crate::time;

pub enum Check {
    New { payload_hash: String },
    Replay { result: Value },
}

/// Canonical hash: serde_json::Value objects are BTreeMap-backed, so keys are
/// serialized in sorted order.
pub fn payload_hash<T: Serialize>(op_type: &str, payload: &T) -> AppResult<String> {
    let v = serde_json::to_value(payload)?;
    let mut h = Sha256::new();
    h.update(op_type.as_bytes());
    h.update([0u8]);
    h.update(v.to_string().as_bytes());
    Ok(hex::encode(h.finalize()))
}

pub fn validate_operation_id(op_id: &str) -> AppResult<()> {
    if op_id.len() < 16 || op_id.len() > 64 || !op_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(AppError::validation("A valid operation id (16–64 characters) is required."));
    }
    Ok(())
}

pub fn check<T: Serialize>(conn: &Connection, op_id: &str, op_type: &str, payload: &T) -> AppResult<Check> {
    validate_operation_id(op_id)?;
    let hash = payload_hash(op_type, payload)?;
    let existing: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT operation_type, payload_hash, result_json FROM operation_idempotency WHERE operation_id = ?1",
            [op_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match existing {
        None => Ok(Check::New { payload_hash: hash }),
        Some((t, h, res)) => {
            if t != op_type || h != hash {
                return Err(AppError::new(
                    ErrorCode::IdempotencyMismatch,
                    "This operation id was already used for a different request. Nothing was changed.",
                )
                .with_details(serde_json::json!({ "operation_id": op_id })));
            }
            let result = match res {
                Some(s) => serde_json::from_str(&s)?,
                None => Value::Null,
            };
            Ok(Check::Replay { result })
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn complete(
    conn: &Connection,
    op_id: &str,
    op_type: &str,
    actor_id: Option<&str>,
    device_id: Option<&str>,
    payload_hash: &str,
    result_reference: Option<&str>,
    result: &Value,
) -> AppResult<()> {
    let now = time::now_str();
    conn.execute(
        "INSERT INTO operation_idempotency(operation_id, operation_type, actor_id, device_id, payload_hash, status,
             result_reference, result_json, created_at, completed_at)
         VALUES (?1,?2,?3,?4,?5,'completed',?6,?7,?8,?8)",
        params![op_id, op_type, actor_id, device_id, payload_hash, result_reference, result.to_string(), now],
    )?;
    Ok(())
}
