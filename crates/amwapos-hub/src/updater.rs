//! Signed updates.
//!
//! The publisher signs a manifest with an Ed25519 key; the matching public key
//! is compiled into the app (`AMWAPOS_UPDATE_PUBKEY`, base64). The feed is
//! `{"payload": "<JSON text>", "signature": "<base64 Ed25519 over payload bytes>"}`
//! and the payload is
//! `{"version","notes","published_at","installer":{"url","sha256","size","file_name"}}`.
//!
//! Rules: no public key → nothing is checked or installed; a bad signature,
//! an older or equal version, or an installer whose size or SHA-256 differ
//! from the signed payload is refused. The installer is hashed again right
//! before it is started, after a safety backup. Installing runs the NSIS
//! installer silently (Windows only); business data is kept by the installer.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::{AppCore, AppError, AppResult, ErrorCode};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const BUILT_IN_PUBKEY: Option<&str> = option_env!("AMWAPOS_UPDATE_PUBKEY");
const MAX_INSTALLER: u64 = 500 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Installer {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub file_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub published_at: String,
    pub installer: Installer,
}

fn refused(msg: impl Into<String>, kind: &str) -> AppError {
    AppError::new(ErrorCode::Conflict, msg.into()).with_details(json!({ "kind": kind }))
}

/// `1.2.10` > `1.2.9`; pre-release suffixes sort before the release.
pub fn newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> (Vec<u64>, bool) {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = match v.split_once('-') {
            Some((c, _)) => (c, true),
            None => (v, false),
        };
        (core.split('.').map(|x| x.parse().unwrap_or(0)).collect(), pre)
    }
    let (a, ap) = parts(candidate);
    let (b, bp) = parts(current);
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    bp && !ap
}

/// Verify the feed document and return the signed manifest.
pub fn verify(feed: &Value, pubkey_b64: Option<&str>) -> AppResult<Manifest> {
    let key_b64 = pubkey_b64.filter(|k| !k.trim().is_empty()).ok_or_else(|| {
        refused("This build has no update-signing key, so updates cannot be verified and are never installed. Install new versions with the signed installer.", "update_unsigned_build")
    })?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let key_bytes: [u8; 32] = b64
        .decode(key_b64.trim())
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| refused("The built-in update key is invalid.", "update_bad_key"))?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| refused("The built-in update key is invalid.", "update_bad_key"))?;
    let payload =
        feed.get("payload").and_then(|p| p.as_str()).ok_or_else(|| refused("The update manifest is not signed.", "update_unsigned"))?;
    let sig_bytes: [u8; 64] = feed
        .get("signature")
        .and_then(|s| s.as_str())
        .and_then(|s| b64.decode(s.trim()).ok())
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| refused("The update manifest is not signed.", "update_unsigned"))?;
    key.verify(payload.as_bytes(), &Signature::from_bytes(&sig_bytes))
        .map_err(|_| refused("The update signature is not valid. The update was refused.", "update_bad_signature"))?;
    let m: Manifest =
        serde_json::from_str(payload).map_err(|_| refused("The signed update manifest is unreadable.", "update_bad_manifest"))?;
    if m.installer.sha256.len() != 64 || !m.installer.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(refused("The signed update manifest has no valid checksum.", "update_bad_manifest"));
    }
    if !(m.installer.url.starts_with("https://") || m.installer.url.starts_with("http://127.0.0.1")) {
        return Err(refused("The installer address must use https://.", "update_bad_manifest"));
    }
    if m.installer.size == 0 || m.installer.size > MAX_INSTALLER {
        return Err(refused("The signed installer size is not plausible.", "update_bad_manifest"));
    }
    let safe = !m.installer.file_name.is_empty()
        && m.installer.file_name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && m.installer.file_name.ends_with(".exe");
    if !safe {
        return Err(refused("The signed installer file name is not allowed.", "update_bad_manifest"));
    }
    Ok(m)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ready {
    manifest: Manifest,
    path: String,
}

pub struct Updater {
    pub pubkey: std::sync::Mutex<Option<String>>,
    state: tokio::sync::Mutex<Option<Manifest>>,
    http: reqwest::Client,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> AppResult<T> + Send + 'static) -> AppResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| AppError::internal(format!("worker failed: {e}")))?
}

fn dir(core: &AppCore) -> PathBuf {
    core.data_dir.join("updates")
}

fn sha256_file(p: &std::path::Path) -> AppResult<(String, u64)> {
    use std::io::Read;
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        n += k as u64;
        h.update(&buf[..k]);
    }
    Ok((hex::encode(h.finalize()), n))
}

impl Updater {
    pub fn new() -> Arc<Updater> {
        Arc::new(Updater {
            pubkey: std::sync::Mutex::new(BUILT_IN_PUBKEY.map(|s| s.to_string())),
            state: tokio::sync::Mutex::new(None),
            http: reqwest::Client::builder().timeout(Duration::from_secs(600)).build().unwrap_or_default(),
        })
    }

    fn key(&self) -> Option<String> {
        self.pubkey.lock().unwrap().clone()
    }

    fn ready(core: &AppCore) -> Option<Ready> {
        let r: Ready = serde_json::from_slice(&std::fs::read(dir(core).join("ready.json")).ok()?).ok()?;
        newer(&r.manifest.version, amwapos_core::audit::APP_VERSION).then_some(r)
    }

    pub async fn status(&self, core: &Arc<AppCore>) -> Value {
        let available = self.state.lock().await.clone();
        json!({
            "current_version": amwapos_core::audit::APP_VERSION,
            "signing_key_built_in": self.key().is_some_and(|k| !k.is_empty()),
            "available": available,
            "downloaded": Self::ready(core).map(|r| r.manifest),
            "can_install": cfg!(windows),
        })
    }

    pub async fn check(&self, core: &Arc<AppCore>, feed_url: &str) -> AppResult<Value> {
        if feed_url.is_empty() {
            return Err(refused("No update address is set. Enter it in Settings → Updates.", "update_no_feed"));
        }
        let key = self.key();
        // Refuse before any network traffic when the build cannot verify.
        verify(&json!({}), key.as_deref()).map(|_| ()).or_else(|e| {
            if e.details.as_ref().and_then(|d| d.get("kind")).and_then(|k| k.as_str()) == Some("update_unsigned_build") {
                Err(e)
            } else {
                Ok(())
            }
        })?;
        let resp = self
            .http
            .get(feed_url)
            .send()
            .await
            .map_err(|e| refused(format!("Could not reach the update server: {e}"), "update_unreachable"))?;
        if !resp.status().is_success() {
            return Err(refused(format!("The update server answered {}.", resp.status()), "update_unreachable"));
        }
        let feed: Value = resp.json().await.map_err(|_| refused("The update manifest is unreadable.", "update_bad_manifest"))?;
        let m = verify(&feed, key.as_deref())?;
        let is_newer = newer(&m.version, amwapos_core::audit::APP_VERSION);
        *self.state.lock().await = if is_newer { Some(m.clone()) } else { None };
        Ok(json!({ "newer": is_newer, "manifest": m, "status": self.status(core).await }))
    }

    /// Download the installer named in the verified manifest and check its
    /// size and SHA-256 against the signed values.
    pub async fn download(&self, core: &Arc<AppCore>) -> AppResult<Value> {
        let m = self.state.lock().await.clone().ok_or_else(|| refused("Check for updates first.", "update_none"))?;
        let d = dir(core);
        std::fs::create_dir_all(&d)?;
        let part = d.join(format!("{}.part", m.installer.file_name));
        let mut resp =
            self.http.get(&m.installer.url).send().await.map_err(|e| refused(format!("Download failed: {e}"), "update_unreachable"))?;
        if !resp.status().is_success() {
            return Err(refused(format!("Download failed ({}).", resp.status()), "update_unreachable"));
        }
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&part)?;
            let mut n = 0u64;
            while let Some(chunk) = resp.chunk().await.map_err(|e| refused(format!("Download failed: {e}"), "update_unreachable"))? {
                n += chunk.len() as u64;
                if n > m.installer.size {
                    drop(f);
                    let _ = std::fs::remove_file(&part);
                    return Err(refused("The installer is larger than the signed size. It was deleted.", "update_hash_mismatch"));
                }
                f.write_all(&chunk)?;
            }
            f.sync_all()?;
        }
        let (sha, size) = {
            let p = part.clone();
            blocking(move || sha256_file(&p)).await?
        };
        if size != m.installer.size || !sha.eq_ignore_ascii_case(&m.installer.sha256) {
            let _ = std::fs::remove_file(&part);
            return Err(refused("The downloaded installer does not match the signed checksum. It was deleted.", "update_hash_mismatch"));
        }
        let final_path = d.join(&m.installer.file_name);
        std::fs::rename(&part, &final_path)?;
        let ready = Ready { manifest: m, path: final_path.to_string_lossy().to_string() };
        std::fs::write(d.join("ready.json"), serde_json::to_vec(&ready)?)?;
        Ok(self.status(core).await)
    }

    /// Verify the downloaded installer again, take a safety backup, then start
    /// the installer. Returns after the installer has been launched.
    pub async fn install(&self, core: &Arc<AppCore>, token: &str) -> AppResult<Value> {
        let r = Self::ready(core).ok_or_else(|| refused("No verified update has been downloaded.", "update_none"))?;
        // The signing key must still be present in this build.
        if self.key().is_none_or(|k| k.is_empty()) {
            return Err(refused("This build has no update-signing key; updates are never installed.", "update_unsigned_build"));
        }
        let path = PathBuf::from(&r.path);
        let (sha, size) = {
            let p = path.clone();
            blocking(move || sha256_file(&p)).await?
        };
        if size != r.manifest.installer.size || !sha.eq_ignore_ascii_case(&r.manifest.installer.sha256) {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(dir(core).join("ready.json"));
            return Err(refused("The downloaded installer changed on disk and was deleted. Download it again.", "update_hash_mismatch"));
        }
        if !cfg!(windows) {
            return Err(refused("Installing updates is supported on Windows only.", "update_platform"));
        }
        let backup = {
            let (c, t) = (core.clone(), token.to_string());
            blocking(move || c.backup_create(&t, None)).await?
        };
        // The installer opens its own window (no silent /S apply): the person
        // sees what is being installed and completes it.
        std::process::Command::new(&path).spawn().map_err(|e| AppError::internal(format!("Could not start the installer: {e}")))?;
        tracing::warn!(version = %r.manifest.version, "update installer started; the app will be replaced");
        Ok(json!({ "started": true, "version": r.manifest.version, "safety_backup": backup }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_order() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.1.10", "0.1.9"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(newer("0.1.0", "0.1.0-soak.1"));
        assert!(!newer("0.1.0-rc.1", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn no_key_means_no_updates() {
        let e = verify(&json!({}), None).unwrap_err();
        assert_eq!(e.details.unwrap()["kind"], "update_unsigned_build");
    }
}
