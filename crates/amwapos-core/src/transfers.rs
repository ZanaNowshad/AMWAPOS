//! Stock locations and transfers (flag `inventory.locations`; transfers
//! between branches also need `org.multi_branch`).
//!
//! A branch's stock level stays the single source of truth for selling.
//! Locations break it down: stock recorded at a non-default location is the
//! sum of the movements tagged with it; the default stockroom holds the rest.
//! A transfer is draft → shipped → received (or cancelled while draft).
//! Shipping writes `transfer_out` movements at the source, receiving writes
//! `transfer_in` at the destination; shipped but not yet received is in
//! transit. Both steps are idempotent on their operation id. Receiving never
//! adds more than was shipped: no stock is created by a transfer.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::auth::Session;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ids::{new_id, next_seq};
use crate::inventory::{apply_movement_at, Movement};
use crate::service::AppCore;
use crate::setup::clean;
use crate::time;
use crate::validate;

/// The branch's default stockroom (created on first use).
pub fn ensure_default_location(c: &Connection, branch_id: &str) -> AppResult<String> {
    if let Some(id) = c
        .query_row("SELECT location_id FROM stock_locations WHERE branch_id=?1 AND is_default=1", [branch_id], |r| r.get::<_, String>(0))
        .optional()?
    {
        return Ok(id);
    }
    let id = format!("LOC-{branch_id}");
    let now = time::now_str();
    c.execute(
        "INSERT INTO stock_locations(location_id, branch_id, code, name, is_default, active, created_at, updated_at)
         VALUES (?1,?2,'STOCKROOM','Stockroom',1,1,?3,?3)",
        params![id, branch_id, now],
    )?;
    Ok(id)
}

/// Quantity of a product at a location.
pub fn location_qty(c: &Connection, product_id: &str, location_id: &str) -> AppResult<i64> {
    let (branch, is_default): (String, i64) =
        c.query_row("SELECT branch_id, is_default FROM stock_locations WHERE location_id=?1", [location_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if is_default == 1 {
        let total = crate::inventory::current_qty(c, product_id, &branch)?;
        let elsewhere: i64 = c.query_row(
            "SELECT COALESCE(SUM(m.qty_delta_milli),0) FROM stock_movements m JOIN stock_locations l ON l.location_id=m.location_id
             WHERE m.product_id=?1 AND m.branch_id=?2 AND l.is_default=0",
            params![product_id, branch],
            |r| r.get(0),
        )?;
        Ok(total - elsewhere)
    } else {
        Ok(c.query_row(
            "SELECT COALESCE(SUM(qty_delta_milli),0) FROM stock_movements WHERE product_id=?1 AND location_id=?2",
            params![product_id, location_id],
            |r| r.get(0),
        )?)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LocationRow {
    pub location_id: String,
    pub branch_id: String,
    pub branch_name: String,
    pub code: String,
    pub name: String,
    pub is_default: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LocationInput {
    pub code: String,
    pub name: String,
    #[serde(default = "yes")]
    pub active: bool,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TransferLineInput {
    pub product_id: String,
    pub qty_milli: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransferInput {
    /// Destination branch (default: the session's branch).
    #[serde(default)]
    pub to_branch_id: Option<String>,
    /// Source location in the session's branch (default: its stockroom).
    #[serde(default)]
    pub from_location_id: Option<String>,
    /// Destination location (default: the destination's stockroom).
    #[serde(default)]
    pub to_location_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    pub lines: Vec<TransferLineInput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransferLineView {
    pub line_no: i64,
    pub product_id: String,
    pub product_name: String,
    pub qty_milli: i64,
    pub qty_received_milli: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TransferView {
    pub transfer_id: String,
    pub transfer_number: String,
    pub from_branch_id: String,
    pub from_branch_name: String,
    pub from_location_id: String,
    pub from_location_name: String,
    pub to_branch_id: String,
    pub to_branch_name: String,
    pub to_location_id: String,
    pub to_location_name: String,
    pub status: String,
    pub note: Option<String>,
    pub created_by_name: Option<String>,
    pub created_at: String,
    pub shipped_at: Option<String>,
    pub received_at: Option<String>,
    pub lines: Vec<TransferLineView>,
}

fn load_transfer(c: &Connection, id: &str) -> AppResult<TransferView> {
    let mut t = c
        .query_row(
            "SELECT t.transfer_id, t.transfer_number, t.from_branch_id, fb.name, t.from_location_id, fl.name, t.to_branch_id, tb.name,
                    t.to_location_id, tl.name, t.status, t.note, u.display_name, t.created_at, t.shipped_at, t.received_at
             FROM stock_transfers t JOIN branches fb ON fb.branch_id=t.from_branch_id JOIN branches tb ON tb.branch_id=t.to_branch_id
             JOIN stock_locations fl ON fl.location_id=t.from_location_id JOIN stock_locations tl ON tl.location_id=t.to_location_id
             LEFT JOIN users u ON u.user_id=t.created_by WHERE t.transfer_id=?1",
            [id],
            |r| {
                Ok(TransferView {
                    transfer_id: r.get(0)?,
                    transfer_number: r.get(1)?,
                    from_branch_id: r.get(2)?,
                    from_branch_name: r.get(3)?,
                    from_location_id: r.get(4)?,
                    from_location_name: r.get(5)?,
                    to_branch_id: r.get(6)?,
                    to_branch_name: r.get(7)?,
                    to_location_id: r.get(8)?,
                    to_location_name: r.get(9)?,
                    status: r.get(10)?,
                    note: r.get(11)?,
                    created_by_name: r.get(12)?,
                    created_at: r.get(13)?,
                    shipped_at: r.get(14)?,
                    received_at: r.get(15)?,
                    lines: vec![],
                })
            },
        )
        .optional()?
        .ok_or_else(|| AppError::not_found("Transfer"))?;
    let mut st = c.prepare(
        "SELECT l.line_no, l.product_id, p.name, l.qty_milli, l.qty_received_milli FROM stock_transfer_lines l
         JOIN products p ON p.product_id=l.product_id WHERE l.transfer_id=?1 ORDER BY l.line_no",
    )?;
    t.lines = st
        .query_map([id], |r| {
            Ok(TransferLineView {
                line_no: r.get(0)?,
                product_id: r.get(1)?,
                product_name: r.get(2)?,
                qty_milli: r.get(3)?,
                qty_received_milli: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(t)
}

/// The location must exist, be active and belong to `branch`.
fn location_in(c: &Connection, location_id: &str, branch: &str) -> AppResult<()> {
    let ok: Option<(String, i64)> = c
        .query_row("SELECT branch_id, active FROM stock_locations WHERE location_id=?1", [location_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    match ok {
        Some((b, 1)) if b == branch => Ok(()),
        Some((b, _)) if b == branch => Err(AppError::validation("That location is inactive.")),
        _ => Err(AppError::validation("That location does not belong to the branch.")),
    }
}

/// One step of a transfer (ship or receive) with its operation id. The same id
/// on the same transfer replays; the same id on another transfer is refused.
fn check_step_op(c: &Connection, col: &str, op: &str, transfer_id: &str) -> AppResult<()> {
    let used: Option<String> =
        c.query_row(&format!("SELECT transfer_id FROM stock_transfers WHERE {col}=?1"), [op], |r| r.get(0)).optional()?;
    match used {
        Some(t) if t != transfer_id => Err(AppError::new(
            ErrorCode::IdempotencyMismatch,
            "This operation id was already used for a different transfer. Nothing was changed.",
        )),
        _ => Ok(()),
    }
}

impl AppCore {
    fn require_locations(&self, s: &Session, perm: &str) -> AppResult<()> {
        s.require(perm)?;
        self.require_feature("inventory.locations")
    }

    pub fn locations_list(&self, token: &str) -> AppResult<Vec<LocationRow>> {
        let s = self.session(token)?;
        self.require_locations(&s, "inventory.view")?;
        let all = self.features()?.is_on("org.multi_branch");
        self.db.write(|tx| {
            ensure_default_location(tx, &s.branch_id)?;
            let mut st = tx.prepare(
                "SELECT l.location_id, l.branch_id, b.name, l.code, l.name, l.is_default, l.active FROM stock_locations l
                 JOIN branches b ON b.branch_id=l.branch_id WHERE (?1 OR l.branch_id=?2) ORDER BY b.name, l.is_default DESC, l.name",
            )?;
            let rows = st
                .query_map(params![all, s.branch_id], |r| {
                    Ok(LocationRow {
                        location_id: r.get(0)?,
                        branch_id: r.get(1)?,
                        branch_name: r.get(2)?,
                        code: r.get(3)?,
                        name: r.get(4)?,
                        is_default: r.get::<_, i64>(5)? == 1,
                        active: r.get::<_, i64>(6)? == 1,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Create or edit a location in the session's branch.
    pub fn location_save(&self, token: &str, location_id: Option<String>, input: LocationInput) -> AppResult<Vec<LocationRow>> {
        let s = self.session(token)?;
        self.require_locations(&s, "inventory.adjust")?;
        let code = clean(&input.code, "Code", 20, true)?.to_uppercase();
        let name = clean(&input.name, "Name", 60, true)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            ensure_default_location(tx, &s.branch_id)?;
            let now = time::now_str();
            match location_id.filter(|x| !x.is_empty()) {
                Some(id) => {
                    let (branch, is_default): (String, i64) = tx
                        .query_row("SELECT branch_id, is_default FROM stock_locations WHERE location_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()?
                        .ok_or_else(|| AppError::not_found("Location"))?;
                    if branch != s.branch_id {
                        return Err(AppError::forbidden("inventory.adjust"));
                    }
                    if is_default == 1 && !input.active {
                        return Err(AppError::validation("The stockroom cannot be deactivated."));
                    }
                    if !input.active {
                        let held: i64 = tx.query_row(
                            "SELECT COALESCE(SUM(qty_delta_milli),0) FROM stock_movements WHERE location_id=?1",
                            [&id],
                            |r| r.get(0),
                        )?;
                        if held != 0 {
                            return Err(AppError::conflict("Move the stock out of this location before deactivating it."));
                        }
                    }
                    tx.execute(
                        "UPDATE stock_locations SET code=?2, name=?3, active=?4, updated_at=?5 WHERE location_id=?1",
                        params![id, code, name, input.active as i64, now],
                    )?;
                    audit::record(tx, &actor, "location.updated", "stock_location", Some(&id), None, Some(&json!({ "code": code, "name": name, "active": input.active })))?;
                }
                None => {
                    let id = new_id();
                    tx.execute(
                        "INSERT INTO stock_locations(location_id, branch_id, code, name, is_default, active, created_at, updated_at) VALUES (?1,?2,?3,?4,0,1,?5,?5)",
                        params![id, s.branch_id, code, name, now],
                    )
                    .map_err(|e| match e {
                        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                            AppError::new(ErrorCode::Duplicate, "A location with this code already exists.")
                        }
                        other => other.into(),
                    })?;
                    audit::record(tx, &actor, "location.created", "stock_location", Some(&id), None, Some(&json!({ "code": code, "name": name })))?;
                }
            }
            Ok(())
        })?;
        self.locations_list(token)
    }

    /// Products held at a location (non-zero quantities).
    pub fn location_stock(&self, token: &str, location_id: &str) -> AppResult<Vec<serde_json::Value>> {
        let s = self.session(token)?;
        self.require_locations(&s, "inventory.view")?;
        let id = validate::id(location_id, "Location")?;
        self.db.read(|c| {
            let branch: String = c
                .query_row("SELECT branch_id FROM stock_locations WHERE location_id=?1", [&id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| AppError::not_found("Location"))?;
            let mut st = c.prepare(
                "SELECT p.product_id, p.name FROM products p
                 WHERE p.product_id IN (SELECT product_id FROM stock_levels WHERE branch_id=?1 AND qty_milli<>0
                                        UNION SELECT product_id FROM stock_movements WHERE location_id=?2)
                 ORDER BY p.name LIMIT 2000",
            )?;
            let prods = st
                .query_map(params![branch, id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            let mut out = vec![];
            for (pid, name) in prods {
                let q = location_qty(c, &pid, &id)?;
                if q != 0 {
                    out.push(json!({ "product_id": pid, "name": name, "qty_milli": q }));
                }
            }
            Ok(out)
        })
    }

    pub fn transfer_create(&self, token: &str, input: TransferInput) -> AppResult<TransferView> {
        let s = self.session(token)?;
        self.require_locations(&s, "inventory.transfer")?;
        let to_branch = input.to_branch_id.clone().filter(|b| !b.is_empty()).unwrap_or_else(|| s.branch_id.clone());
        if to_branch != s.branch_id && !self.features()?.is_on("org.multi_branch") {
            return Err(AppError::new(ErrorCode::Conflict, "Transfers between branches need the multi-branch module.")
                .with_details(json!({ "kind": "feature_disabled", "feature": "org.multi_branch" })));
        }
        if input.lines.is_empty() || input.lines.len() > 500 {
            return Err(AppError::validation("A transfer needs 1 to 500 lines."));
        }
        let note = input.note.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(|n| n.chars().take(300).collect::<String>());
        let actor = self.actor(&s, None);
        let id = self.db.write(|tx| {
            let exists: bool = tx.query_row("SELECT 1 FROM branches WHERE branch_id=?1 AND active=1", [&to_branch], |_| Ok(true)).optional()?.unwrap_or(false);
            if !exists {
                return Err(AppError::not_found("Branch"));
            }
            let from_loc = match input.from_location_id.clone().filter(|x| !x.is_empty()) {
                Some(l) => l,
                None => ensure_default_location(tx, &s.branch_id)?,
            };
            let to_loc = match input.to_location_id.clone().filter(|x| !x.is_empty()) {
                Some(l) => l,
                None => ensure_default_location(tx, &to_branch)?,
            };
            location_in(tx, &from_loc, &s.branch_id)?;
            location_in(tx, &to_loc, &to_branch)?;
            if from_loc == to_loc {
                return Err(AppError::validation("Choose a different destination."));
            }
            let id = new_id();
            let number = format!("TR-{:05}", next_seq(tx, "stock_transfer")?);
            let now = time::now_str();
            tx.execute(
                "INSERT INTO stock_transfers(transfer_id, transfer_number, from_branch_id, from_location_id, to_branch_id, to_location_id, status, note,
                    created_by, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,'draft',?7,?8,?9,?9)",
                params![id, number, s.branch_id, from_loc, to_branch, to_loc, note, s.user_id, now],
            )?;
            for (i, l) in input.lines.iter().enumerate() {
                let pid = validate::id(&l.product_id, "Product")?;
                let (dec, track): (i64, i64) = tx
                    .query_row("SELECT allow_decimal_quantity, track_inventory FROM products WHERE product_id=?1", [&pid], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?
                    .ok_or_else(|| AppError::not_found("Product"))?;
                if track == 0 {
                    return Err(AppError::validation("Only products with stock tracking can be transferred."));
                }
                validate::qty_positive(l.qty_milli, dec == 1, "Quantity")?;
                tx.execute(
                    "INSERT INTO stock_transfer_lines(transfer_id, line_no, product_id, qty_milli) VALUES (?1,?2,?3,?4)",
                    params![id, i as i64 + 1, pid, l.qty_milli],
                )?;
            }
            audit::record(tx, &actor, "transfer.created", "stock_transfer", Some(&id), None, Some(&json!({ "number": number, "to_branch": to_branch, "lines": input.lines.len() })))?;
            Ok(id)
        })?;
        self.db.read(|c| load_transfer(c, &id))
    }

    /// Ship from the session's branch: stock leaves the source location now.
    pub fn transfer_ship(&self, token: &str, transfer_id: &str, operation_id: &str) -> AppResult<TransferView> {
        let s = self.session(token)?;
        self.require_locations(&s, "inventory.transfer")?;
        let id = validate::id(transfer_id, "Transfer")?;
        crate::idempotency::validate_operation_id(operation_id)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            check_step_op(tx, "ship_operation_id", operation_id, &id)?;
            let t = load_transfer(tx, &id)?;
            if t.from_branch_id != s.branch_id {
                return Err(AppError::new(ErrorCode::Forbidden, "Only the sending branch can ship this transfer."));
            }
            let prior: Option<String> = tx.query_row("SELECT ship_operation_id FROM stock_transfers WHERE transfer_id=?1", [&id], |r| r.get(0))?;
            if t.status != "draft" {
                return if prior.as_deref() == Some(operation_id) { Ok(()) } else { Err(AppError::conflict(format!("This transfer is already {}.", t.status))) };
            }
            for l in &t.lines {
                let have = crate::transfers::location_qty(tx, &l.product_id, &t.from_location_id)?;
                if have < l.qty_milli {
                    return Err(AppError::new(
                        ErrorCode::InsufficientStock,
                        format!("Not enough {} at {} (have {}).", l.product_name, t.from_location_name, crate::money::format_qty(have)),
                    ));
                }
                apply_movement_at(
                    tx,
                    &Movement {
                        product_id: &l.product_id,
                        branch_id: &t.from_branch_id,
                        kind: "transfer_out",
                        qty_delta_milli: -l.qty_milli,
                        unit_cost_minor: None,
                        source_type: "stock_transfer",
                        source_id: Some(&id),
                        reason: Some(&t.transfer_number),
                        user_id: Some(&s.user_id),
                        device_id: Some(&s.device_id),
                    },
                    Some(&t.from_location_id),
                )?;
            }
            let now = time::now_str();
            tx.execute(
                "UPDATE stock_transfers SET status='shipped', ship_operation_id=?2, shipped_by=?3, shipped_at=?4, updated_at=?4 WHERE transfer_id=?1",
                params![id, operation_id, s.user_id, now],
            )?;
            audit::record(tx, &actor, "transfer.shipped", "stock_transfer", Some(&id), None, Some(&json!({ "number": t.transfer_number })))?;
            Ok(())
        })?;
        self.db.read(|c| load_transfer(c, &id))
    }

    /// Receive at the destination branch: exactly what was shipped arrives.
    pub fn transfer_receive(&self, token: &str, transfer_id: &str, operation_id: &str) -> AppResult<TransferView> {
        let s = self.session(token)?;
        self.require_locations(&s, "inventory.transfer")?;
        let id = validate::id(transfer_id, "Transfer")?;
        crate::idempotency::validate_operation_id(operation_id)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            check_step_op(tx, "receive_operation_id", operation_id, &id)?;
            let t = load_transfer(tx, &id)?;
            if t.to_branch_id != s.branch_id {
                return Err(AppError::new(ErrorCode::Forbidden, "Only the receiving branch can receive this transfer."));
            }
            let prior: Option<String> = tx.query_row("SELECT receive_operation_id FROM stock_transfers WHERE transfer_id=?1", [&id], |r| r.get(0))?;
            if t.status != "shipped" {
                return if prior.as_deref() == Some(operation_id) {
                    Ok(())
                } else if t.status == "draft" {
                    Err(AppError::conflict("This transfer has not been shipped yet."))
                } else {
                    Err(AppError::conflict(format!("This transfer is already {}.", t.status)))
                };
            }
            for l in &t.lines {
                apply_movement_at(
                    tx,
                    &Movement {
                        product_id: &l.product_id,
                        branch_id: &t.to_branch_id,
                        kind: "transfer_in",
                        qty_delta_milli: l.qty_milli,
                        unit_cost_minor: None,
                        source_type: "stock_transfer",
                        source_id: Some(&id),
                        reason: Some(&t.transfer_number),
                        user_id: Some(&s.user_id),
                        device_id: Some(&s.device_id),
                    },
                    Some(&t.to_location_id),
                )?;
                tx.execute(
                    "UPDATE stock_transfer_lines SET qty_received_milli=qty_milli WHERE transfer_id=?1 AND line_no=?2",
                    params![id, l.line_no],
                )?;
            }
            let now = time::now_str();
            tx.execute(
                "UPDATE stock_transfers SET status='received', receive_operation_id=?2, received_by=?3, received_at=?4, updated_at=?4 WHERE transfer_id=?1",
                params![id, operation_id, s.user_id, now],
            )?;
            audit::record(tx, &actor, "transfer.received", "stock_transfer", Some(&id), None, Some(&json!({ "number": t.transfer_number })))?;
            Ok(())
        })?;
        self.db.read(|c| load_transfer(c, &id))
    }

    pub fn transfer_cancel(&self, token: &str, transfer_id: &str) -> AppResult<TransferView> {
        let s = self.session(token)?;
        self.require_locations(&s, "inventory.transfer")?;
        let id = validate::id(transfer_id, "Transfer")?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let t = load_transfer(tx, &id)?;
            if t.from_branch_id != s.branch_id {
                return Err(AppError::new(ErrorCode::Forbidden, "Only the sending branch can cancel this transfer."));
            }
            if t.status != "draft" {
                return Err(AppError::conflict("Only a draft transfer can be cancelled; a shipped one is received at its destination."));
            }
            tx.execute("UPDATE stock_transfers SET status='cancelled', updated_at=?2 WHERE transfer_id=?1", params![id, time::now_str()])?;
            audit::record(tx, &actor, "transfer.cancelled", "stock_transfer", Some(&id), None, None)?;
            Ok(())
        })?;
        self.db.read(|c| load_transfer(c, &id))
    }

    /// Transfers touching the session's branch (all branches with `branches.all`).
    pub fn transfers_list(&self, token: &str, status: Option<String>) -> AppResult<Vec<TransferView>> {
        let s = self.session(token)?;
        self.require_locations(&s, "inventory.view")?;
        let all = s.has("branches.all");
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT transfer_id FROM stock_transfers WHERE (?1 IS NULL OR status=?1) AND (?2 OR from_branch_id=?3 OR to_branch_id=?3)
                 ORDER BY created_at DESC LIMIT 300",
            )?;
            let ids = st
                .query_map(params![status.filter(|x| !x.is_empty()), all, s.branch_id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids.iter().map(|id| load_transfer(c, id)).collect()
        })
    }

    pub fn transfer_get(&self, token: &str, transfer_id: &str) -> AppResult<TransferView> {
        let s = self.session(token)?;
        self.require_locations(&s, "inventory.view")?;
        let id = validate::id(transfer_id, "Transfer")?;
        let t = self.db.read(|c| load_transfer(c, &id))?;
        if !s.has("branches.all") && t.from_branch_id != s.branch_id && t.to_branch_id != s.branch_id {
            return Err(AppError::forbidden("branches.all"));
        }
        Ok(t)
    }

    /// Quantities shipped and not yet received, per product and destination.
    pub fn transfers_in_transit(&self, token: &str) -> AppResult<Vec<serde_json::Value>> {
        let s = self.session(token)?;
        self.require_locations(&s, "inventory.view")?;
        let all = s.has("branches.all");
        self.db.read(|c| {
            let mut st = c.prepare(
                "SELECT l.product_id, p.name, t.to_branch_id, b.name, SUM(l.qty_milli - l.qty_received_milli)
                 FROM stock_transfer_lines l JOIN stock_transfers t ON t.transfer_id=l.transfer_id
                 JOIN products p ON p.product_id=l.product_id JOIN branches b ON b.branch_id=t.to_branch_id
                 WHERE t.status='shipped' AND (?1 OR t.from_branch_id=?2 OR t.to_branch_id=?2)
                 GROUP BY l.product_id, t.to_branch_id ORDER BY p.name",
            )?;
            let rows = st
                .query_map(params![all, s.branch_id], |r| {
                    Ok(json!({ "product_id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "to_branch_id": r.get::<_, String>(2)?,
                               "to_branch_name": r.get::<_, String>(3)?, "qty_milli": r.get::<_, i64>(4)? }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }
}
