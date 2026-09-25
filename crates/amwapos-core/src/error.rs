//! Structured application errors.
//!
//! Every error that crosses the command boundary answers three questions for
//! the operator: what happened (`code` + `message`), whether data was changed
//! (`data_changed`), and whether retrying the same request is safe
//! (`retryable`). Messages never include secrets.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Input failed validation.
    Validation,
    /// Referenced entity does not exist.
    NotFound,
    /// No valid session.
    Unauthenticated,
    /// Session is valid but lacks permission.
    Forbidden,
    /// Action is allowed only with a manager approval token.
    ApprovalRequired,
    /// Wrong PIN.
    InvalidCredentials,
    /// Account is locked because of failed attempts or deactivation.
    AccountLocked,
    /// State conflict (e.g. cart already finalized, shift already open).
    Conflict,
    /// An idempotency key was reused with a different payload.
    IdempotencyMismatch,
    /// An operation with this key is still in progress.
    OperationInProgress,
    /// Stock would go negative where not allowed.
    InsufficientStock,
    /// The application has not been set up yet.
    NotSetUp,
    /// An open shift is required for this action.
    ShiftRequired,
    /// Database is busy after bounded retries.
    DatabaseBusy,
    /// Database integrity / corruption problem.
    DatabaseCorrupt,
    /// Other database failure.
    Database,
    /// File system failure.
    Io,
    /// Printer failure (never used to undo a sale).
    Printer,
    /// Hub / sync failure.
    Sync,
    /// Duplicate unique value (barcode, SKU...).
    Duplicate,
    /// Not enough disk space for the operation.
    InsufficientDisk,
    /// The bundled OCR models are missing or damaged; OCR stays off.
    OcrModelMissing,
    /// Unexpected internal failure.
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    /// True only if the failed operation may have persisted changes.
    #[serde(default)]
    pub data_changed: bool,
    /// True if retrying the identical request is safe and may succeed.
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let retryable = matches!(code, ErrorCode::DatabaseBusy | ErrorCode::OperationInProgress | ErrorCode::Sync);
        Self { code, message: message.into(), data_changed: false, retryable, details: None }
    }
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::Validation, msg)
    }
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, format!("{} was not found.", what.into()))
    }
    pub fn forbidden(permission: &str) -> Self {
        Self::new(ErrorCode::Forbidden, "You do not have permission to perform this action.")
            .with_details(serde_json::json!({ "permission": permission }))
    }
    pub fn approval_required(permission: &str, summary: impl Into<String>) -> Self {
        Self::new(ErrorCode::ApprovalRequired, "Manager approval required.")
            .with_details(serde_json::json!({ "permission": permission, "summary": summary.into() }))
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, msg)
    }
    pub fn duplicate(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::Duplicate, msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, msg)
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        use rusqlite::ffi::ErrorCode as C;
        if let rusqlite::Error::SqliteFailure(f, ref msg) = e {
            match f.code {
                C::DatabaseBusy | C::DatabaseLocked => {
                    return AppError::new(ErrorCode::DatabaseBusy, "The database is busy. No changes were recorded; please try again.")
                }
                C::DatabaseCorrupt | C::NotADatabase => {
                    return AppError::new(
                        ErrorCode::DatabaseCorrupt,
                        "The database file appears to be damaged. Open Diagnostics or restore a backup.",
                    )
                }
                C::ConstraintViolation => {
                    let m = msg.clone().unwrap_or_default();
                    if m.contains("UNIQUE") {
                        return AppError::duplicate(format!("A record with this value already exists ({m})."));
                    }
                    return AppError::validation(format!("Data constraint violated ({m})."));
                }
                C::DiskFull => return AppError::new(ErrorCode::InsufficientDisk, "The disk is full. No changes were recorded."),
                _ => {}
            }
        }
        if let rusqlite::Error::QueryReturnedNoRows = e {
            return AppError::not_found("Record");
        }
        tracing::error!(error = %e, "database error");
        AppError::new(ErrorCode::Database, format!("Database error: {e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::new(ErrorCode::Io, format!("File system error: {e}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::validation(format!("Invalid data: {e}"))
    }
}
