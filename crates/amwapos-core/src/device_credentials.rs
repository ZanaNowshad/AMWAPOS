//! Per-device credential rotation and revocation (Wave 7,
//! docs/OPERATIONAL_CONTROL.md).
//!
//! A terminal's hub credential is derived from the hub's master secret (in
//! the operating system's credential store) and a credential version kept on
//! the device row. Version 1 is the derivation every terminal paired before
//! Wave 7 already uses, so existing credentials keep working unchanged.
//!
//! Rotation is a staged handshake that is safe for a terminal that is
//! offline for days:
//! 1. A person with `devices.manage` starts it on the hub. The current
//!    credential keeps working; the next version is staged.
//! 2. At the terminal's next heartbeat (signed and sealed with its current
//!    credential) the hub hands over the new one, encrypted on the wire.
//! 3. The terminal stores it in its own credential store and signs from then
//!    on with it, keeping the previous one only until the hub confirms.
//! 4. The first request signed with the new version is the proof: the hub
//!    makes it current. The previous version is accepted for
//!    [`GRACE_MINUTES`] more minutes (requests already on their way), then
//!    never again. There are never two permanent credentials and no global
//!    fallback secret.
//!
//! A rotation not picked up within [`STALE_HOURS`] hours opens a case. It can
//! be cancelled. Revoking is separate: a revoked terminal is refused at once;
//! revoking a lost or stolen terminal also moves its credential version on,
//! so re-activating that device can never bring the old credential back.
//! Resetting every terminal (a new master secret) is owner-only and needs a
//! typed confirmation.
//!
//! No key is ever written to the database, a log, an audit record, a case,
//! a diagnostic or an assistant answer: only version numbers and times.

use chrono::Duration;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::audit;
use crate::auth;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::service::AppCore;
use crate::sync::{self, hmac_hex, signing_string, verify_hex_eq, NonceCache};
use crate::time;
use crate::validate;

/// How long the previous credential is still accepted after the terminal
/// proved the new one.
pub const GRACE_MINUTES: i64 = 10;
/// A staged rotation not picked up for this long opens a case.
pub const STALE_HOURS: i64 = 24;
/// The words an owner types to reset every terminal's credential.
pub const RESET_ALL_PHRASE: &str = "RESET ALL TERMINALS";

/// Where a terminal keeps the credential it used before a rotation, until
/// the hub confirms the new one.
pub const SECRET_DEVICE_KEY_PREV: &str = "amwapos.sync.device_key.previous";

/// The credential for a device at a version. Version 1 is the pre-Wave 7
/// derivation (unchanged), so existing terminals keep working.
pub fn derive_key(master: &str, device_id: &str, version: i64) -> String {
    if version <= 1 {
        hmac_hex(master, format!("device:{device_id}").as_bytes())
    } else {
        hmac_hex(master, format!("device:{device_id}:v{version}").as_bytes())
    }
}

/// A device's credential state (versions and times only).
#[derive(Debug, Clone)]
struct CredState {
    active: bool,
    version: i64,
    next: Option<i64>,
    prev: Option<i64>,
    grace_until: Option<String>,
}

impl AppCore {
    fn cred_state(&self, id: &str) -> AppResult<Option<CredState>> {
        self.db.read(|c| {
            Ok(c.query_row(
                "SELECT active, credential_version, credential_next_version, credential_prev_version, credential_grace_until
                 FROM devices WHERE device_id=?1",
                [id],
                |r| {
                    Ok(CredState {
                        active: r.get::<_, i64>(0)? == 1,
                        version: r.get(1)?,
                        next: r.get(2)?,
                        prev: r.get(3)?,
                        grace_until: r.get(4)?,
                    })
                },
            )
            .optional()?)
        })
    }

    /// The credential at one version (hub side).
    pub fn hub_device_key_version(&self, device_id: &str, version: i64) -> AppResult<String> {
        Ok(derive_key(&self.hub_master_secret()?, device_id, version))
    }

    /// A device's current credential (hub side): used at pairing.
    pub fn hub_device_key(&self, device_id: &str) -> AppResult<String> {
        let v = self.cred_state(device_id)?.map(|s| s.version).unwrap_or(1);
        self.hub_device_key_version(device_id, v)
    }

    /// Authenticate a signed request. Returns the active device id.
    #[allow(clippy::too_many_arguments)]
    pub fn hub_authenticate(
        &self,
        nonces: &NonceCache,
        device_id: &str,
        ts: i64,
        nonce: &str,
        signature: &str,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> AppResult<String> {
        self.hub_authenticate_key(nonces, device_id, ts, nonce, signature, method, path, body).map(|(id, _)| id)
    }

    /// Authenticate a signed request and return the device id with the
    /// credential that signed it (the reply is sealed with the same one).
    /// Accepts the current version; the previous one only inside its grace;
    /// a staged one, which then becomes current (the terminal proved it).
    #[allow(clippy::too_many_arguments)]
    pub fn hub_authenticate_key(
        &self,
        nonces: &NonceCache,
        device_id: &str,
        ts: i64,
        nonce: &str,
        signature: &str,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> AppResult<(String, String)> {
        let d = self.require_device()?;
        if d.mode != "hub" {
            return Err(AppError::conflict("This computer is not running as a hub."));
        }
        let id = validate::id(device_id, "Device")?;
        let now = time::now();
        if (now.timestamp_millis() - ts).abs() > 5 * 60 * 1000 {
            return Err(AppError::new(
                ErrorCode::Unauthenticated,
                "Request timestamp outside the allowed window. Check the terminal clock.",
            ));
        }
        if nonce.len() < 16 || nonce.len() > 64 {
            return Err(AppError::new(ErrorCode::Unauthenticated, "Invalid nonce."));
        }
        let st = match self.cred_state(&id)? {
            None => return Err(AppError::new(ErrorCode::Unauthenticated, "Unknown terminal. Pair it with the hub again.")),
            Some(s) if !s.active => {
                return Err(AppError::new(
                    ErrorCode::Forbidden,
                    "This terminal has been revoked. Ask the owner to re-activate or re-pair it.",
                ))
            }
            Some(s) => s,
        };
        let master = self.hub_master_secret()?;
        let in_grace = st.grace_until.as_deref().map(|g| g > time::fmt(now).as_str()).unwrap_or(false);
        let mut candidates = vec![st.version];
        if let Some(n) = st.next {
            candidates.push(n);
        }
        if let (Some(p), true) = (st.prev, in_grace) {
            candidates.push(p);
        }
        let signed = signing_string(method, path, ts, nonce, body);
        let mut matched = None;
        for v in candidates {
            let key = derive_key(&master, &id, v);
            if verify_hex_eq(&hmac_hex(&key, signed.as_bytes()), signature) {
                matched = Some((v, key));
                break;
            }
        }
        let Some((version, key)) = matched else {
            return Err(AppError::new(ErrorCode::Unauthenticated, "Invalid request signature."));
        };
        if !nonces.check_and_insert(&id, nonce, ts) {
            return Err(AppError::new(ErrorCode::Unauthenticated, "Replayed request rejected."));
        }
        if Some(version) == st.next {
            // The terminal proved the new credential: it becomes current. Only
            // one request makes the change (the update is conditional).
            self.db.write(|tx| {
                let grace = time::fmt(now + Duration::minutes(GRACE_MINUTES));
                let n = tx.execute(
                    "UPDATE devices SET credential_prev_version=credential_version, credential_version=?2, credential_next_version=NULL,
                        credential_staged_at=NULL, credential_grace_until=?3, credential_rotated_at=?4
                     WHERE device_id=?1 AND credential_next_version=?2",
                    params![id, version, grace, time::fmt(now)],
                )?;
                if n == 1 {
                    audit::record(
                        tx,
                        &audit::Actor::default(),
                        "device.credential_rotated",
                        "device",
                        Some(&id),
                        Some(&json!({ "version": st.version })),
                        Some(&json!({ "version": version, "previous_accepted_until": grace })),
                    )?;
                }
                Ok(())
            })?;
        }
        Ok((id, key))
    }

    fn require_terminal_row(&self, id: &str) -> AppResult<()> {
        let mode: Option<String> =
            self.db.read(|c| Ok(c.query_row("SELECT operating_mode FROM devices WHERE device_id=?1", [id], |r| r.get(0)).optional()?))?;
        match mode.as_deref() {
            Some("terminal") => Ok(()),
            Some(_) => Err(AppError::conflict("Only a terminal's credential can be rotated or revoked here.")),
            None => Err(AppError::not_found("Device")),
        }
    }

    /// Start a credential rotation for one terminal. The current credential
    /// keeps working until the terminal proves the new one. Starting again
    /// while one is staged changes nothing.
    pub fn device_rotate_credential(&self, token: &str, device_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("devices.manage")?;
        self.require_back_office_writable()?;
        let d = self.require_device()?;
        if d.mode != "hub" {
            return Err(AppError::conflict("Credentials are managed on the hub."));
        }
        let id = validate::id(device_id, "Device")?;
        self.require_terminal_row(&id)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let (active, version, next): (i64, i64, Option<i64>) =
                tx.query_row("SELECT active, credential_version, credential_next_version FROM devices WHERE device_id=?1", [&id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
            if active != 1 {
                return Err(AppError::conflict("This terminal is revoked. A revoked terminal is paired again, not rotated."));
            }
            if let Some(n) = next {
                return Ok(json!({ "device_id": id, "version": version, "next_version": n, "already_staged": true }));
            }
            let n = version + 1;
            tx.execute(
                "UPDATE devices SET credential_next_version=?2, credential_staged_at=?3 WHERE device_id=?1",
                params![id, n, time::now_str()],
            )?;
            audit::record(
                tx,
                &actor,
                "device.credential_rotation_started",
                "device",
                Some(&id),
                Some(&json!({ "version": version })),
                Some(&json!({ "next_version": n })),
            )?;
            Ok(json!({ "device_id": id, "version": version, "next_version": n, "already_staged": false }))
        })
    }

    /// Cancel a staged rotation (the terminal has not proved it yet). If the
    /// terminal already received the new credential, it falls back to the
    /// current one by itself.
    pub fn device_cancel_rotation(&self, token: &str, device_id: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("devices.manage")?;
        self.require_back_office_writable()?;
        let id = validate::id(device_id, "Device")?;
        self.require_terminal_row(&id)?;
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let next: Option<i64> = tx.query_row("SELECT credential_next_version FROM devices WHERE device_id=?1", [&id], |r| r.get(0))?;
            let Some(n) = next else { return Ok(json!({ "device_id": id, "cancelled": false })) };
            tx.execute("UPDATE devices SET credential_next_version=NULL, credential_staged_at=NULL WHERE device_id=?1", [&id])?;
            audit::record(
                tx,
                &actor,
                "device.credential_rotation_cancelled",
                "device",
                Some(&id),
                Some(&json!({ "next_version": n })),
                None,
            )?;
            Ok(json!({ "device_id": id, "cancelled": true }))
        })
    }

    /// Revoke a terminal: refused at once. `lost_or_stolen` also moves its
    /// credential on so the device can never be re-activated with it.
    pub fn device_revoke(&self, token: &str, device_id: &str, reason: &str, lost_or_stolen: bool) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("devices.manage")?;
        self.require_back_office_writable()?;
        let id = validate::id(device_id, "Device")?;
        self.require_terminal_row(&id)?;
        let reason = reason.trim();
        if reason.chars().count() < 3 {
            return Err(AppError::validation("Say why this terminal is revoked."));
        }
        let actor = self.actor(&s, None);
        self.db.write(|tx| {
            let now = time::now_str();
            let version: i64 = tx.query_row("SELECT credential_version FROM devices WHERE device_id=?1", [&id], |r| r.get(0))?;
            let new_version = if lost_or_stolen { version + 1 } else { version };
            tx.execute(
                "UPDATE devices SET active=0, revoked_at=?2, credential_next_version=NULL, credential_staged_at=NULL,
                    credential_prev_version=NULL, credential_grace_until=NULL, credential_version=?3,
                    revocation_reason=CASE WHEN ?4 THEN ?5 ELSE revocation_reason END
                 WHERE device_id=?1",
                params![id, now, new_version, lost_or_stolen, reason],
            )?;
            audit::record(
                tx,
                &actor,
                "device.revoked",
                "device",
                Some(&id),
                None,
                Some(&json!({ "reason": reason, "lost_or_stolen": lost_or_stolen, "credential_version": new_version })),
            )?;
            Ok(json!({ "device_id": id, "revoked": true, "lost_or_stolen": lost_or_stolen }))
        })
    }

    /// Emergency: replace the hub's master secret. Every terminal's
    /// credential stops working and every terminal must be paired again.
    /// Owner only, with the typed confirmation [`RESET_ALL_PHRASE`].
    pub fn sync_reset_hub_credentials(&self, token: &str, confirm: &str) -> AppResult<Value> {
        let s = self.session(token)?;
        s.require("sync.manage")?;
        if s.role_id != auth::ROLE_OWNER {
            return Err(AppError::new(ErrorCode::Forbidden, "Only the owner can reset every terminal's credential."));
        }
        if confirm.trim() != RESET_ALL_PHRASE {
            return Err(AppError::validation(
                "Type RESET ALL TERMINALS to confirm. Every terminal will stop syncing until it is paired again.",
            ));
        }
        let d = self.require_device()?;
        if d.mode != "hub" {
            return Err(AppError::conflict("This computer is not running as a hub."));
        }
        let secret = auth::random_token();
        let actor = self.actor(&s, None);
        let n = self.db.read(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM devices WHERE operating_mode='terminal' AND active=1", [], |r| r.get::<_, i64>(0))?)
        })?;
        self.secrets.set(sync::SECRET_HUB_MASTER, &secret)?;
        self.db.write(|tx| {
            sync::record_hub_secret_fingerprint(tx, &secret, Some(&s.user_id))?;
            tx.execute(
                "UPDATE devices SET credential_next_version=NULL, credential_staged_at=NULL, credential_prev_version=NULL, credential_grace_until=NULL",
                [],
            )?;
            audit::record(tx, &actor, "sync.hub_credentials_reset", "device", Some(&d.device_id), None, Some(&json!({ "terminals": n })))?;
            Ok(())
        })?;
        Ok(json!({ "ok": true, "terminals_must_pair_again": true, "terminals": n }))
    }

    /// Hub: what a heartbeat reply says about the terminal's credential. The
    /// staged credential travels only inside the sealed reply.
    pub(crate) fn heartbeat_credential(&self, device_id: &str) -> AppResult<Value> {
        let Some(st) = self.cred_state(device_id)? else { return Ok(Value::Null) };
        let mut out = json!({ "version": st.version });
        if let Some(n) = st.next {
            out["next"] = json!({ "version": n, "key": self.hub_device_key_version(device_id, n)? });
        }
        Ok(out)
    }

    /// Terminal: act on the credential part of a heartbeat reply. Install a
    /// staged credential (keeping the current one until the hub confirms the
    /// new one), or drop the previous one once confirmed. Returns what
    /// happened (never the key).
    pub(crate) fn terminal_apply_credential(&self, cred: &Value) -> AppResult<&'static str> {
        let installed = self.terminal_sync_settings()?.credential_version.unwrap_or(1);
        let current = cred["version"].as_i64().unwrap_or(installed);
        if let (Some(v), Some(key)) = (cred["next"]["version"].as_i64(), cred["next"]["key"].as_str()) {
            if v > installed && key.len() >= 32 {
                let old = self.terminal_device_key()?;
                self.secrets.set(SECRET_DEVICE_KEY_PREV, &old)?;
                self.secrets.set(sync::SECRET_DEVICE_KEY, key)?;
                self.set_terminal_credential_version(v, Some(installed))?;
                return Ok("installed");
            }
        }
        if current == installed && self.secrets.get(SECRET_DEVICE_KEY_PREV)?.is_some() {
            self.secrets.delete(SECRET_DEVICE_KEY_PREV)?;
            return Ok("confirmed");
        }
        Ok("unchanged")
    }

    /// Terminal: the hub refused the credential this terminal signs with.
    /// If a previous one is still kept (a rotation the hub did not complete),
    /// go back to it. Returns whether it did.
    pub fn terminal_credential_fallback(&self) -> AppResult<bool> {
        let Some(prev) = self.secrets.get(SECRET_DEVICE_KEY_PREV)? else { return Ok(false) };
        let ss = self.terminal_sync_settings()?;
        self.secrets.set(sync::SECRET_DEVICE_KEY, &prev)?;
        self.secrets.delete(SECRET_DEVICE_KEY_PREV)?;
        let back = ss.credential_prev_version.unwrap_or(1);
        self.set_terminal_credential_version(back, None)?;
        Ok(true)
    }
}
