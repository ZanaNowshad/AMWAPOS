//! AMWAPOS core: domain rules, persistence and application services.
//!
//! Layering: `commands` (transport-neutral dispatch) → `service`/feature
//! modules (auth, validation, transactions) → pure domain (`money`,
//! `pricing`) and repositories (SQL inside feature modules).

pub mod ai;
pub mod audit;
pub mod auth;
pub mod backup;
pub mod branches;
pub mod catalog;
pub mod channel;
pub mod commands;
pub mod credit;
pub mod customers;
pub mod db;
pub mod error;
pub mod idempotency;
pub mod ids;
pub mod importer;
pub mod inventory;
pub mod loyalty;
pub mod messaging;
pub mod migration;
pub mod money;
pub mod ocrflow;
pub mod orders;
pub mod pdf;
pub mod pos;
pub mod pricing;
pub mod printing;
pub mod purchasing;
pub mod raster;
pub mod receipt;
pub mod refunds;
pub mod reports;
pub mod sales;
pub mod service;
pub mod settings;
pub mod setup;
pub mod shifts;
pub mod sync;
pub mod system;
pub mod time;
pub mod transfers;
pub mod users;
pub mod validate;

pub use error::{AppError, AppResult, ErrorCode};
pub use service::AppCore;
