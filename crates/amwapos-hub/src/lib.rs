//! AMWAPOS networking: LAN hub API, terminal sync client, discovery and the
//! runtime that starts the right services for the device's mode.

pub mod client;
pub mod discovery;
pub mod runtime;
pub mod server;

pub use runtime::Runtime;
