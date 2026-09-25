//! AMWAPOS networking: LAN hub API, terminal sync client, discovery and the
//! runtime that starts the right services for the device's mode.

pub mod ai_client;
pub mod client;
pub mod discovery;
pub mod ocr_worker;
pub mod runtime;
pub mod server;
pub mod updater;
pub mod whatsapp;

pub use runtime::{Runtime, StepUp};
