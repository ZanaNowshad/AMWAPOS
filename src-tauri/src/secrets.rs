//! Secrets in the operating system's credential store (Windows Credential
//! Manager on Windows). Secret values are never logged.

use amwapos_core::service::SecretStore;
use amwapos_core::{AppError, AppResult, ErrorCode};

const SERVICE: &str = "AMWAPOS";

pub struct OsSecretStore;

fn err(e: keyring::Error) -> AppError {
    AppError::new(ErrorCode::Io, format!("The Windows credential store is unavailable ({e})."))
}

impl SecretStore for OsSecretStore {
    fn get(&self, key: &str) -> AppResult<Option<String>> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(err)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(err(e)),
        }
    }
    fn set(&self, key: &str, value: &str) -> AppResult<()> {
        keyring::Entry::new(SERVICE, key).map_err(err)?.set_password(value).map_err(err)
    }
    fn delete(&self, key: &str) -> AppResult<()> {
        match keyring::Entry::new(SERVICE, key).map_err(err)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(err(e)),
        }
    }
}
