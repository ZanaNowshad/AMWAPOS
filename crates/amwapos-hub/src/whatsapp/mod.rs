//! WhatsApp, in-process: `WhatsAppService` (feature `whatsapp.enabled`,
//! default off) → `WhatsAppAdapter` → `RustWhatsAppAdapter` (the unofficial
//! `whatsapp-rust` Web client), `WahaAdapter` (a WAHA gateway over REST,
//! chosen in Settings → WhatsApp) or `FakeAdapter` (tests). No Node process,
//! no local HTTP port.

pub mod adapter;
pub mod catalog;
pub mod catalog_proto;
pub mod fake;
pub mod rust_adapter;
pub mod service;
pub mod session;
pub mod waha;

pub use adapter::{AdapterSession, CatalogCapability, CatalogProduct, RemoteProduct, WaContact, WhatsAppAdapter};
pub use fake::FakeAdapter;
pub use rust_adapter::RustWhatsAppAdapter;
pub use service::{WaStatus, WhatsAppService};
pub use session::SessionPath;
pub use waha::{WahaAdapter, WahaConfig};
