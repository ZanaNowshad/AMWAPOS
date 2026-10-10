//! Rider cash custody.
//!
//! A rider who takes cash at the door records it on the ticket (the customer
//! has paid) but the money is in no drawer yet: the collection row carries
//! `held_by`. At the till a cashier opens **Rider hand-over**, ticks any
//! delivered cash drops the rider did not record themselves, counts what the
//! rider hands over, and the counted amount enters this shift's drawer. The
//! difference against what the records say the rider holds is kept as the
//! hand-over's variance.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::idempotency::{self, Check};
use crate::ids::{new_id, next_seq};
use crate::service::AppCore;
use crate::setup::clean_opt;
use crate::time;
use crate::validate;

#[derive(Debug, Clone, Serialize)]
pub struct RiderCash {
    pub rider_user_id: String,
    pub name: String,
    /// Cash the rider recorded at the door, not yet handed over.
    pub held: Vec<Value>,
    pub held_minor: i64,
    /// Drops given to the rider that are out or delivered with cash still to
    /// collect; the cashier can tick them at the hand-over.
    pub uncollected: Vec<Value>,
    pub uncollected_minor: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HandoverRequest {
    pub rider_user_id: String,
    /// Drops the rider collected cash for without recording it.
    #[serde(default)]
    pub collect: Vec<String>,
    pub counted_minor: i64,
    #[serde(default)]
    pub note: Option<String>,
    pub operation_id: String,
}

/// Cash each rider holds (held collections not handed over), all branches.
pub(crate) fn held_by_rider(c: &Connection) -> AppResult<Vec<Value>> {
    let mut st = c.prepare(
        "SELECT k.held_by, COALESCE(u.display_name,''), COUNT(*), SUM(k.amount_minor), MIN(k.created_at)
         FROM sale_collections k LEFT JOIN users u ON u.user_id=k.held_by
         WHERE k.held_by IS NOT NULL AND k.collection_id NOT IN (SELECT collection_id FROM rider_handover_items)
         GROUP BY k.held_by ORDER BY 4 DESC",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(json!({ "rider_user_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "drops": r.get::<_, i64>(2)?,
                "amount_minor": r.get::<_, i64>(3)?, "since": r.get::<_, String>(4)? }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

fn can_hand_over(s: &Session) -> AppResult<()> {
    if s.has("pos.sell") || s.has("deliveries.manage") {
        Ok(())
    } else {
        Err(AppError::forbidden("pos.sell"))
    }
}

pub(crate) fn rider_cash(c: &Connection, rider: &str, name: &str, branch: &str) -> AppResult<RiderCash> {
    let mut st = c.prepare(
        "SELECT k.collection_id, k.delivery_id, COALESCE(s.receipt_number, d.delivery_number), cu.name, d.area, k.amount_minor, k.created_at
         FROM sale_collections k LEFT JOIN delivery_orders d ON d.delivery_id=k.delivery_id LEFT JOIN sales s ON s.sale_id=k.sale_id
         LEFT JOIN customers cu ON cu.customer_id=d.customer_id
         WHERE k.held_by=?1 AND k.collection_id NOT IN (SELECT collection_id FROM rider_handover_items) ORDER BY k.created_at",
    )?;
    let held: Vec<Value> = st
        .query_map([rider], |r| {
            Ok(json!({ "collection_id": r.get::<_, String>(0)?, "delivery_id": r.get::<_, Option<String>>(1)?,
                "number": r.get::<_, Option<String>>(2)?, "customer_name": r.get::<_, Option<String>>(3)?, "area": r.get::<_, Option<String>>(4)?,
                "amount_minor": r.get::<_, i64>(5)?, "at": r.get::<_, String>(6)? }))
        })?
        .collect::<Result<_, _>>()?;
    let held_minor = held.iter().filter_map(|v| v["amount_minor"].as_i64()).sum();
    let mut st = c.prepare(
        "SELECT d.delivery_id FROM delivery_orders d WHERE d.assigned_user_id=?1 AND d.status IN ('dispatched','delivered')
           AND COALESCE(d.branch_id, ?2)=?2 AND COALESCE(d.pay_state,'unpaid') NOT IN ('paid','recorded') ORDER BY d.created_at",
    )?;
    let ids: Vec<String> = st.query_map(params![rider, branch], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let mut uncollected = vec![];
    for id in ids {
        let t = crate::tickets::load_ticket(c, &id)?;
        if t.outstanding_minor > 0 {
            uncollected.push(json!({ "delivery_id": id, "number": t.number, "customer_name": t.customer_name, "area": t.area,
                "status": t.status, "outstanding_minor": t.outstanding_minor }));
        }
    }
    let uncollected_minor = uncollected.iter().filter_map(|v| v["outstanding_minor"].as_i64()).sum();
    Ok(RiderCash { rider_user_id: rider.to_string(), name: name.to_string(), held, held_minor, uncollected, uncollected_minor })
}

impl AppCore {
    /// Riders with cash to hand over at this till: held cash plus drops out
    /// or delivered with pay-on-delivery money still to collect.
    pub fn rider_cash_list(&self, token: &str) -> AppResult<Vec<RiderCash>> {
        let s = self.session(token)?;
        can_hand_over(&s)?;
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT DISTINCT u.user_id, u.display_name FROM users u
                 WHERE u.user_id IN (SELECT held_by FROM sale_collections WHERE held_by IS NOT NULL
                                       AND collection_id NOT IN (SELECT collection_id FROM rider_handover_items))
                    OR u.user_id IN (SELECT assigned_user_id FROM delivery_orders WHERE assigned_user_id IS NOT NULL
                                       AND status IN ('dispatched','delivered') AND COALESCE(pay_state,'unpaid') NOT IN ('paid','recorded'))
                 ORDER BY u.display_name COLLATE NOCASE",
            )?;
            let riders: Vec<(String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            let mut out = vec![];
            for (id, name) in riders {
                let rc = rider_cash(c, &id, &name, &s.branch_id)?;
                if rc.held_minor > 0 || rc.uncollected_minor > 0 {
                    out.push(rc);
                }
            }
            Ok(out)
        })
    }

    /// Count a rider's cash into this shift's drawer. Idempotent on
    /// `operation_id`.
    pub fn rider_handover(&self, token: &str, req: HandoverRequest) -> AppResult<Value> {
        let s = self.session(token)?;
        can_hand_over(&s)?;
        let rider = validate::id(&req.rider_user_id, "Rider")?;
        if rider == s.user_id {
            return Err(AppError::new(crate::error::ErrorCode::Forbidden, "Another person must count your cash in."));
        }
        validate::money_non_negative(req.counted_minor, "Counted cash")?;
        let note = clean_opt(&req.note, "Note", 300)?;
        let mut collect: Vec<String> = req.collect.iter().map(|d| validate::id(d, "Ticket")).collect::<AppResult<_>>()?;
        collect.sort();
        collect.dedup();
        let device = self.require_device()?;
        let actor = self.actor(&s, None);
        let payload = json!({ "rider": rider, "collect": collect, "counted_minor": req.counted_minor, "note": note });
        let handover_id = self.db.write(|tx| {
            let hash = match idempotency::check(tx, &req.operation_id, "rider.handover", &payload)? {
                Check::Replay { result } => return Ok(result["handover_id"].as_str().unwrap_or_default().to_string()),
                Check::New { payload_hash } => payload_hash,
            };
            let shift = crate::sales::open_shift_for(tx, &s)?.ok_or_else(crate::sales::shift_required)?;
            let now = time::now_str();
            // Drops the rider collected for without recording it: record the
            // cash now, as held by the rider, so it is handed over below.
            for id in &collect {
                let t = crate::tickets::load_ticket(tx, id)?;
                if t.assigned_user_id.as_deref() != Some(rider.as_str()) {
                    return Err(AppError::conflict(format!("Ticket {} is not given to this rider.", t.number)));
                }
                if !matches!(t.status.as_str(), "dispatched" | "delivered") || t.outstanding_minor <= 0 {
                    return Err(AppError::conflict(format!("Ticket {} has nothing to collect.", t.number)));
                }
                let cid = new_id();
                tx.execute(
                    "INSERT INTO sale_collections(collection_id, sale_id, delivery_id, method, amount_minor, reference, shift_id, branch_id, device_id,
                        user_id, operation_id, created_at, held_by) VALUES (?1,?2,?3,'cash',?4,NULL,NULL,?5,?6,?7,?8,?9,?10)",
                    params![cid, t.sale_id, id, t.outstanding_minor, s.branch_id, device.device_id, s.user_id, new_id(), now, rider],
                )?;
                tx.execute("UPDATE delivery_orders SET pay_state='paid', payment_status='paid', updated_at=?2 WHERE delivery_id=?1", params![id, now])?;
                tx.execute(
                    "INSERT INTO delivery_events(event_id, delivery_id, previous_status, new_status, note, user_id, created_at) VALUES (?1,?2,?3,?3,?4,?5,?6)",
                    params![new_id(), id, t.status, format!("Cash collected by the rider: {} (counted at hand-over)", t.outstanding_minor), s.user_id, now],
                )?;
            }
            let mut st = tx.prepare(
                "SELECT collection_id, amount_minor FROM sale_collections
                 WHERE held_by=?1 AND collection_id NOT IN (SELECT collection_id FROM rider_handover_items)",
            )?;
            let items: Vec<(String, i64)> = st.query_map([&rider], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            drop(st);
            if items.is_empty() {
                return Err(AppError::conflict("This rider has no cash to hand over."));
            }
            let expected: i64 = items.iter().map(|i| i.1).sum();
            let variance = req.counted_minor - expected;
            if variance != 0 && note.is_none() {
                return Err(AppError::validation("The count does not match. Add a note saying why.")
                    .with_details(json!({ "expected_minor": expected, "variance_minor": variance })));
            }
            let hid = new_id();
            let number = format!("{}-H{:05}", device.device_code, next_seq(tx, &format!("handover:{}", device.device_id))?);
            tx.execute(
                "INSERT INTO rider_handovers(handover_id, handover_number, rider_user_id, expected_minor, counted_minor, variance_minor, note, shift_id,
                    branch_id, device_id, user_id, operation_id, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![hid, number, rider, expected, req.counted_minor, variance, note, shift, s.branch_id, device.device_id, s.user_id, req.operation_id, now],
            )?;
            for (cid, amt) in &items {
                tx.execute(
                    "INSERT INTO rider_handover_items(collection_id, handover_id, amount_minor) VALUES (?1,?2,?3)",
                    params![cid, hid, amt],
                )?;
            }
            audit::record(
                tx,
                &actor,
                "rider.handover",
                "rider_handover",
                Some(&hid),
                None,
                Some(&json!({ "rider": rider, "expected_minor": expected, "counted_minor": req.counted_minor, "variance_minor": variance,
                    "collections": items.len(), "recorded_now": collect.len(), "shift_id": shift })),
            )?;
            if req.counted_minor > 0 {
                crate::printing::enqueue_drawer_pulse(tx, Some(&s.user_id), &hid)?;
            }
            idempotency::complete(
                tx,
                &req.operation_id,
                "rider.handover",
                Some(&s.user_id),
                Some(&device.device_id),
                &hash,
                Some(&hid),
                &json!({ "handover_id": hid }),
            )?;
            Ok(hid)
        })?;
        self.db.read(|c| {
            Ok(c.query_row(
                "SELECT h.handover_number, h.rider_user_id, u.display_name, h.expected_minor, h.counted_minor, h.variance_minor, h.created_at,
                    (SELECT COUNT(*) FROM rider_handover_items i WHERE i.handover_id=h.handover_id)
                 FROM rider_handovers h LEFT JOIN users u ON u.user_id=h.rider_user_id WHERE h.handover_id=?1",
                [&handover_id],
                |r| {
                    Ok(json!({ "handover_id": handover_id, "handover_number": r.get::<_, String>(0)?, "rider_user_id": r.get::<_, String>(1)?,
                        "rider_name": r.get::<_, Option<String>>(2)?, "expected_minor": r.get::<_, i64>(3)?, "counted_minor": r.get::<_, i64>(4)?,
                        "variance_minor": r.get::<_, i64>(5)?, "at": r.get::<_, String>(6)?, "drops": r.get::<_, i64>(7)? }))
                },
            )?)
        })
    }
}
