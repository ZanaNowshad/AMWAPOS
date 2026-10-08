//! Terminal Health (Wave 7, docs/OPERATIONAL_CONTROL.md): one view of every
//! paired terminal built only from what the hub observed.
//!
//! * Each fact comes from one place: what the terminal reported in its last
//!   heartbeat (`device_heartbeats`), the shifts it sent (`shifts`), the
//!   records the hub refused (`sync_dead_letters`), open cases. A fact a
//!   terminal never reported is shown as unknown, never filled in.
//! * The app, database and sync protocol versions are shown separately.
//! * "Not seen recently" after [`ops::NOT_SEEN_HEALTH_MIN`] minutes; it only
//!   needs attention while the till has an open shift (a till switched off
//!   after closing is offline, not broken).
//! * Backups are a store fact (the hub's), not a terminal's. Print failures
//!   are evidence from this computer's print queue: terminals do not report
//!   theirs, so their printers are unknown here.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::error::AppResult;
use crate::ops;
use crate::service::AppCore;
use crate::sync::PROTOCOL_VERSION;
use crate::time;

/// What the hub knows about one terminal.
#[derive(Debug, Clone, Default)]
pub struct Observed {
    pub active: bool,
    pub last_seen_at: Option<String>,
    pub schema_version: Option<i64>,
    pub protocol_version: Option<i64>,
    pub pending: Option<i64>,
    pub oldest_pending_at: Option<String>,
    pub last_push_at: Option<String>,
    pub refused_on_hub: i64,
    pub refused_reported: Option<i64>,
    pub last_error: Option<String>,
    pub open_shift: bool,
}

/// The verdict: overall health, connection, and the reasons behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    /// healthy | attention | offline | unknown | revoked
    pub health: &'static str,
    /// online | not_seen_recently | never_reported | revoked
    pub connection: &'static str,
    pub reasons: Vec<&'static str>,
    /// Facts the terminal has not reported (shown as unknown).
    pub unknown: Vec<&'static str>,
}

/// Decide a terminal's health from what was observed. Pure: the same facts
/// and time give the same answer.
pub fn assess(o: &Observed, now: DateTime<Utc>, hub_schema: i64) -> Assessment {
    let mut unknown = vec![];
    if o.protocol_version.is_none() {
        unknown.push("protocol_version");
    }
    if o.oldest_pending_at.is_none() && o.pending.unwrap_or(0) > 0 {
        unknown.push("oldest_pending_at");
    }
    if o.refused_reported.is_none() {
        unknown.push("refused_reported");
    }
    if !o.active {
        return Assessment { health: "revoked", connection: "revoked", reasons: vec![], unknown };
    }
    let Some(seen) = o.last_seen_at.as_deref() else {
        return Assessment { health: "unknown", connection: "never_reported", reasons: vec!["never_reported"], unknown };
    };
    let minutes = time::parse(seen).map(|t| (now - t).num_minutes()).unwrap_or(i64::MAX);
    let connection = if minutes >= ops::NOT_SEEN_HEALTH_MIN { "not_seen_recently" } else { "online" };
    let mut reasons = vec![];
    if connection == "not_seen_recently" && o.open_shift {
        reasons.push("not_seen_during_shift");
    }
    if o.protocol_version.map(|p| p != PROTOCOL_VERSION).unwrap_or(false) {
        reasons.push("protocol_mismatch");
    }
    if o.schema_version.map(|s| s != hub_schema).unwrap_or(false) {
        reasons.push("schema_mismatch");
    }
    if ops::is_backlog(o.pending.unwrap_or(0), o.oldest_pending_at.as_deref(), o.last_push_at.as_deref(), now) {
        reasons.push("backlog");
    }
    if o.refused_on_hub > 0 || o.refused_reported.unwrap_or(0) > 0 {
        reasons.push("refused_records");
    }
    if o.last_error.is_some() {
        reasons.push("sync_error");
    }
    let health = if !reasons.is_empty() {
        "attention"
    } else if connection == "not_seen_recently" {
        "offline"
    } else {
        "healthy"
    };
    Assessment { health, connection, reasons, unknown }
}

impl AppCore {
    /// Every terminal's health, with the store facts beside it. For people
    /// with `devices.manage` or `sync.manage`.
    pub fn terminals_health(&self, token: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        if !s.has("devices.manage") {
            s.require("sync.manage")?;
        }
        let now = time::now();
        let hub_schema = crate::db::latest_schema_version();
        let this = self.device();
        let mode = this.as_ref().map(|d| d.mode.clone()).unwrap_or_else(|| "standalone".into());
        let backup = self.backup_diagnostic().ok();
        let rows = self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT d.device_id, d.name, d.device_code, d.active, d.activated_at, d.revoked_at, b.name,
                        h.device_id IS NOT NULL, h.last_seen_at, h.last_heartbeat_at, h.app_version, h.schema_version, h.protocol_version,
                        h.pending_count, h.oldest_pending_at, h.last_push_at, h.last_pull_at, h.last_sync_ok_at, h.last_error,
                        h.problem_count, u.display_name,
                        (SELECT COUNT(*) FROM sync_dead_letters l WHERE l.status='open' AND l.origin=d.device_id),
                        (SELECT COUNT(*) FROM cases k WHERE k.device_id=d.device_id AND k.status NOT IN ('resolved','dismissed')),
                        x.shift_id, x.shift_number, x.opened_at, xu.display_name
                 FROM devices d
                 LEFT JOIN branches b ON b.branch_id=d.branch_id
                 LEFT JOIN device_heartbeats h ON h.device_id=d.device_id
                 LEFT JOIN users u ON u.user_id=h.current_user_id
                 LEFT JOIN shifts x ON x.shift_id=(SELECT y.shift_id FROM shifts y WHERE y.device_id=d.device_id AND y.status='open'
                                                   ORDER BY y.opened_at DESC LIMIT 1)
                 LEFT JOIN users xu ON xu.user_id=x.user_id
                 WHERE d.operating_mode='terminal'
                 ORDER BY d.device_code LIMIT 500",
            )?;
            let rows = st
                .query_map([], |r| {
                    let o = Observed {
                        active: r.get::<_, i64>(3)? == 1,
                        last_seen_at: r.get(8)?,
                        schema_version: r.get(11)?,
                        protocol_version: r.get(12)?,
                        pending: r.get(13)?,
                        oldest_pending_at: r.get(14)?,
                        last_push_at: r.get(15)?,
                        refused_on_hub: r.get(21)?,
                        refused_reported: r.get(19)?,
                        last_error: r.get(18)?,
                        open_shift: r.get::<_, Option<String>>(23)?.is_some(),
                    };
                    let a = assess(&o, now, hub_schema);
                    let minutes = o.last_seen_at.as_deref().and_then(|t| time::parse(t).ok()).map(|t| (now - t).num_minutes());
                    let shift = match r.get::<_, Option<String>>(23)? {
                        Some(id) => json!({ "shift_id": id, "shift_number": r.get::<_, Option<String>>(24)?,
                            "opened_at": r.get::<_, Option<String>>(25)?, "user": r.get::<_, Option<String>>(26)? }),
                        None => Value::Null,
                    };
                    Ok(json!({
                        "device_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "code": r.get::<_, String>(2)?,
                        "active": o.active, "paired_at": r.get::<_, String>(4)?, "revoked_at": r.get::<_, Option<String>>(5)?,
                        "branch": r.get::<_, Option<String>>(6)?,
                        "reported": r.get::<_, bool>(7)?,
                        "health": a.health, "connection": a.connection, "reasons": a.reasons, "unknown": a.unknown,
                        "last_seen_at": o.last_seen_at, "minutes_since_seen": minutes,
                        "last_heartbeat_at": r.get::<_, Option<String>>(9)?,
                        "versions": {
                            "app": { "reported": r.get::<_, Option<String>>(10)?, "hub": crate::audit::APP_VERSION },
                            "schema": { "reported": o.schema_version, "hub": hub_schema,
                                "matches": o.schema_version.map(|v| v == hub_schema) },
                            "protocol": { "reported": o.protocol_version, "hub": PROTOCOL_VERSION,
                                "matches": o.protocol_version.map(|v| v == PROTOCOL_VERSION) },
                        },
                        "sending": { "pending": o.pending, "oldest_pending_at": o.oldest_pending_at, "last_push_at": o.last_push_at,
                            "last_pull_at": r.get::<_, Option<String>>(16)?, "last_sync_ok_at": r.get::<_, Option<String>>(17)?,
                            "last_error": o.last_error },
                        "refused": { "on_hub": o.refused_on_hub, "reported_by_till": o.refused_reported },
                        "signed_in": r.get::<_, Option<String>>(20)?,
                        "open_cases": r.get::<_, i64>(22)?,
                        "shift": shift,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        let print = self.db.read(|c| {
            let since = time::fmt(now - chrono::Duration::hours(24));
            Ok(c.query_row(
                "SELECT COUNT(*) FILTER (WHERE status='failed'), MAX(CASE WHEN status='failed' THEN updated_at END),
                        MAX(CASE WHEN status='printed' THEN updated_at END)
                 FROM print_jobs WHERE kind<>'drawer' AND updated_at>=?1",
                [&since],
                |r| {
                    Ok(json!({ "failed_24h": r.get::<_, i64>(0)?, "last_failed_at": r.get::<_, Option<String>>(1)?,
                        "last_printed_at": r.get::<_, Option<String>>(2)? }))
                },
            )?)
        })?;
        let mut counts = std::collections::BTreeMap::<&str, i64>::new();
        for r in &rows {
            *counts
                .entry(match r["health"].as_str() {
                    Some("healthy") => "healthy",
                    Some("attention") => "attention",
                    Some("offline") => "offline",
                    Some("revoked") => "revoked",
                    _ => "unknown",
                })
                .or_default() += 1;
        }
        Ok(json!({
            "mode": mode,
            "thresholds": { "not_seen_minutes": ops::NOT_SEEN_HEALTH_MIN, "not_seen_case_minutes": ops::NOT_SEEN_CASE_MIN,
                "backlog_count": ops::BACKLOG_COUNT, "backlog_minutes": ops::BACKLOG_STALE_MIN },
            "hub": { "app_version": crate::audit::APP_VERSION, "schema_version": hub_schema, "protocol_version": PROTOCOL_VERSION },
            "store": {
                "backup": backup.map(|b| json!({ "state": b.state, "summary": b.summary })),
                "printing_here": print,
            },
            "counts": counts,
            "terminals": rows,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(min_ago: i64, now: DateTime<Utc>) -> Option<String> {
        Some(time::fmt(now - chrono::Duration::minutes(min_ago)))
    }

    #[test]
    fn unknown_stays_unknown_and_offline_after_closing_is_not_a_problem() {
        let now = time::now();
        let hub = 35;
        let base = Observed { active: true, last_seen_at: at(1, now), schema_version: Some(hub), ..Default::default() };
        let a = assess(&base, now, hub);
        assert_eq!((a.health, a.connection), ("healthy", "online"));
        assert!(a.unknown.contains(&"protocol_version") && a.unknown.contains(&"refused_reported"));
        // Never reported.
        let a = assess(&Observed { active: true, ..Default::default() }, now, hub);
        assert_eq!((a.health, a.connection), ("unknown", "never_reported"));
        // Switched off after closing vs silent during a shift.
        let off = Observed { last_seen_at: at(90, now), ..base.clone() };
        assert_eq!(assess(&off, now, hub).health, "offline");
        let silent = Observed { open_shift: true, ..off.clone() };
        assert_eq!(assess(&silent, now, hub).reasons, vec!["not_seen_during_shift"]);
        // Revoked wins.
        assert_eq!(assess(&Observed { active: false, ..silent }, now, hub).health, "revoked");
    }

    #[test]
    fn versions_backlog_and_refusals_are_separate_reasons() {
        let now = time::now();
        let hub = 35;
        let o = Observed {
            active: true,
            last_seen_at: at(1, now),
            schema_version: Some(34),
            protocol_version: Some(PROTOCOL_VERSION + 1),
            pending: Some(3),
            oldest_pending_at: at(20, now),
            refused_on_hub: 2,
            refused_reported: Some(0),
            ..Default::default()
        };
        let a = assess(&o, now, hub);
        assert_eq!(a.health, "attention");
        assert_eq!(a.reasons, vec!["protocol_mismatch", "schema_mismatch", "backlog", "refused_records"]);
        // A few waiting records that are fresh are not a backlog; a terminal
        // that does not report its oldest record needs the count rule.
        let fresh = Observed { oldest_pending_at: at(2, now), ..o.clone() };
        assert!(!assess(&fresh, now, hub).reasons.contains(&"backlog"));
        let old_style = Observed { oldest_pending_at: None, pending: Some(60), last_push_at: at(30, now), ..o };
        assert!(assess(&old_style, now, hub).reasons.contains(&"backlog"));
    }
}
