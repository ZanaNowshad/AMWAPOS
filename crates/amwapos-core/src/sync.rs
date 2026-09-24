//! Multi-terminal synchronization (hub ⇄ terminal). See module docs below.

use serde_json::json;

use crate::error::AppResult;
use crate::service::AppCore;
use crate::system::DiagnosticItem;

impl AppCore {
    pub(crate) fn sync_diagnostic(&self) -> AppResult<DiagnosticItem> {
        let mode = self.device().map(|d| d.mode).unwrap_or_else(|| "standalone".into());
        let pending: i64 = self.db.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| r.get(0))?))?;
        Ok(DiagnosticItem {
            component: "Sync".into(),
            state: "info".into(),
            summary: format!("Mode: {mode}"),
            details: json!({ "mode": mode, "outbox_entries": pending }),
        })
    }
}
