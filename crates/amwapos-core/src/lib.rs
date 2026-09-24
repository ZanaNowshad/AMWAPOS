//! AMWAPOS core: domain rules, persistence and application services.
//!
//! Layering: `commands` (transport-neutral dispatch) → `service`/feature
//! modules (auth, validation, transactions) → pure domain (`money`,
//! `pricing`) and repositories (SQL inside feature modules).

pub mod audit;
pub mod auth;
pub mod db;
pub mod error;
pub mod idempotency;
pub mod ids;
pub mod money;
pub mod pricing;
pub mod service;
pub mod settings;
pub mod setup;
pub mod time;

pub use error::{AppError, AppResult, ErrorCode};
pub use service::AppCore;
