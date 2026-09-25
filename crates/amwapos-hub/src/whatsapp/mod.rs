//! WhatsApp, in-process: `WhatsAppService` (feature `whatsapp.enabled`,
//! default off) → `WhatsAppAdapter` → `RustWhatsAppAdapter` (the unofficial
//! `whatsapp-rust` Web client) or `FakeAdapter` (tests). No Node process, no
//! local HTTP port.

pub mod adapter;
pub mod fake;
pub mod rust_adapter;
pub mod service;
pub mod session;

pub use adapter::{AdapterSession, WhatsAppAdapter};
pub use fake::FakeAdapter;
pub use rust_adapter::RustWhatsAppAdapter;
pub use service::{WaStatus, WhatsAppService};
pub use session::SessionPath;
