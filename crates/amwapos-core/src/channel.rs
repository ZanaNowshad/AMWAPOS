//! Encrypted hub ↔ terminal channel (sync protocol 2).
//!
//! * **Pairing:** SPAKE2 (Ed25519 group) keyed by the hub's pairing code. A
//!   passive listener learns nothing, and an active attacker gets exactly one
//!   online guess per attempt (the hub rate-limits and burns the code). The
//!   pairing request and response, which carry the device key and the bootstrap
//!   snapshot, are sealed with keys derived from the SPAKE2 secret.
//! * **After pairing:** every request and response body is sealed with
//!   ChaCha20-Poly1305 under direction-specific keys derived (HKDF-SHA256) from
//!   the per-device key. The associated data binds method, path, device,
//!   timestamp and nonce, so a sealed body cannot be replayed onto another
//!   request.
//!
//! Sealed format: `nonce (12 bytes) || ciphertext+tag`.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::ChaCha20Poly1305;
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::error::{AppError, AppResult, ErrorCode};

pub type Key = [u8; 32];

/// Header every protocol-2 request carries.
pub const PROTOCOL_HEADER: &str = "x-amw-protocol";
const ID_TERMINAL: &[u8] = b"amwapos-terminal";
const ID_HUB: &[u8] = b"amwapos-hub";

pub fn derive(ikm: &[u8], salt: &[u8], info: &str) -> Key {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(Some(salt), ikm).expand(info.as_bytes(), &mut out).expect("32 bytes is a valid HKDF length");
    out
}

pub fn seal(key: &Key, aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = ChaCha20Poly1305::new(key.into())
        .encrypt((&nonce).into(), Payload { msg: plain, aad })
        .expect("ChaCha20-Poly1305 encryption cannot fail for in-memory buffers");
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

pub fn open(key: &Key, aad: &[u8], sealed: &[u8]) -> AppResult<Vec<u8>> {
    if sealed.len() < 12 + 16 {
        return Err(AppError::new(ErrorCode::Unauthenticated, "Encrypted message is truncated."));
    }
    let (nonce, ct) = sealed.split_at(12);
    ChaCha20Poly1305::new(key.into())
        .decrypt(nonce.into(), Payload { msg: ct, aad })
        .map_err(|_| AppError::new(ErrorCode::Unauthenticated, "Encrypted message could not be verified."))
}

/// Direction keys for a paired device: (terminal→hub, hub→terminal).
pub fn device_keys(device_key: &str) -> (Key, Key) {
    (derive(device_key.as_bytes(), b"amwapos/2/device", "t2h"), derive(device_key.as_bytes(), b"amwapos/2/device", "h2t"))
}

/// Associated data for a request (`method != "RESPONSE"`) or its response.
pub fn aad(kind: &str, path: &str, device_id: &str, ts: i64, nonce: &str) -> Vec<u8> {
    format!("amwapos/2\n{kind}\n{path}\n{device_id}\n{ts}\n{nonce}").into_bytes()
}

/// The SPAKE2 password is the SHA-256 of the pairing code, which is what the
/// hub stores; the code itself never leaves the terminal.
fn pake_password(code: &str) -> Password {
    Password::new(crate::auth::sha256_hex(code.trim()).into_bytes())
}

/// Keys agreed during pairing.
pub struct PairKeys {
    pub request: Key,
    pub response: Key,
}

fn pair_keys(secret: &[u8], pairing_id: &str) -> PairKeys {
    PairKeys { request: derive(secret, pairing_id.as_bytes(), "pair-t2h"), response: derive(secret, pairing_id.as_bytes(), "pair-h2t") }
}

/// Terminal side of pairing.
pub struct TerminalPairing {
    state: Spake2<Ed25519Group>,
    pub pairing_id: String,
    pub message: Vec<u8>,
}

impl TerminalPairing {
    pub fn start(code: &str) -> Self {
        let (state, message) = Spake2::<Ed25519Group>::start_a(&pake_password(code), &Identity::new(ID_TERMINAL), &Identity::new(ID_HUB));
        Self { state, pairing_id: ulid::Ulid::new().to_string(), message }
    }

    pub fn finish(self, hub_message: &[u8]) -> AppResult<PairKeys> {
        let secret =
            self.state.finish(hub_message).map_err(|_| AppError::new(ErrorCode::Sync, "The hub sent an invalid pairing message."))?;
        Ok(pair_keys(&secret, &self.pairing_id))
    }
}

/// Hub side of pairing. `code_hash` is the stored SHA-256 of the active code.
pub struct HubPairing {
    state: Spake2<Ed25519Group>,
    pub code_hash: String,
}

impl HubPairing {
    pub fn start(code_hash: &str) -> (Self, Vec<u8>) {
        let (state, message) =
            Spake2::<Ed25519Group>::start_b(&Password::new(code_hash.as_bytes()), &Identity::new(ID_TERMINAL), &Identity::new(ID_HUB));
        (Self { state, code_hash: code_hash.to_string() }, message)
    }

    pub fn finish(self, terminal_message: &[u8], pairing_id: &str) -> AppResult<PairKeys> {
        let secret =
            self.state.finish(terminal_message).map_err(|_| AppError::new(ErrorCode::InvalidCredentials, "Invalid pairing message."))?;
        Ok(pair_keys(&secret, pairing_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_roundtrip_and_tamper_detection() {
        let k = derive(b"secret", b"salt", "info");
        let a = aad("POST", "/sync/push", "dev", 1, "n");
        let s = seal(&k, &a, b"hello sales");
        assert!(!s.windows(5).any(|w| w == b"hello"));
        assert_eq!(open(&k, &a, &s).unwrap(), b"hello sales");
        // Different request metadata, key or modified bytes → rejected.
        assert!(open(&k, &aad("POST", "/sync/pull", "dev", 1, "n"), &s).is_err());
        assert!(open(&derive(b"other", b"salt", "info"), &a, &s).is_err());
        let mut bad = s.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(open(&k, &a, &bad).is_err());
        // Two seals of the same plaintext differ (random nonce).
        assert_ne!(seal(&k, &a, b"x"), seal(&k, &a, b"x"));
    }

    fn run(terminal_code: &str, hub_code_hash: &str) -> (PairKeys, PairKeys) {
        let t = TerminalPairing::start(terminal_code);
        let (h, hub_msg) = HubPairing::start(hub_code_hash);
        let (tmsg, id) = (t.message.clone(), t.pairing_id.clone());
        (t.finish(&hub_msg).unwrap(), h.finish(&tmsg, &id).unwrap())
    }

    #[test]
    fn pairing_agrees_only_with_the_right_code() {
        let hash = crate::auth::sha256_hex("12345678");
        let (t1, h1) = run("12345678", &hash);
        assert_eq!(t1.request, h1.request);
        assert_eq!(t1.response, h1.response);
        let (t2, _) = run("12345678", &hash);
        assert_ne!(t1.request, t2.request, "fresh keys per pairing");

        let (tw, hw) = run("87654321", &hash);
        let sealed = seal(&tw.request, b"pair", b"{}");
        assert!(open(&hw.request, b"pair", &sealed).is_err(), "wrong code must not decrypt");
    }
}
