//! Where the WhatsApp session lives, and its (explicit) backup.
//!
//! The session (device identity and Signal keys) is kept in its own SQLite
//! file, `<data>/whatsapp/session.db`, where `<data>` is the AMWAPOS data
//! folder (`%ProgramData%\AMWAPOS\data` on Windows). It is never the sales
//! ledger and never a file next to the program. Only the WhatsApp adapter
//! opens it; AMWAPOS' own connections never do (the explicit backup below
//! opens its own read-only connection).

use std::path::{Path, PathBuf};

use amwapos_core::{AppError, AppResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPath(PathBuf);

impl SessionPath {
    /// The session file for a data folder. Refuses paths that would put the
    /// session in the ledger file or next to the executable.
    pub fn for_data_dir(data_dir: &Path) -> AppResult<Self> {
        let p = data_dir.join("whatsapp").join("session.db");
        Self::check(&p, &data_dir.join(amwapos_core::service::DB_FILE))?;
        Ok(Self(p))
    }

    /// The path without checks, for reporting a refused location.
    pub(crate) fn unchecked(data_dir: &Path) -> Self {
        Self(data_dir.join("whatsapp").join("session.db"))
    }

    fn check(p: &Path, ledger: &Path) -> AppResult<()> {
        if !p.is_absolute() {
            return Err(AppError::internal("The WhatsApp session path must be absolute."));
        }
        if p == ledger {
            return Err(AppError::internal("The WhatsApp session must not share the sales database."));
        }
        if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            if p.parent() == Some(exe_dir.as_path()) {
                return Err(AppError::internal("The WhatsApp session must not be stored next to the program."));
            }
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn dir(&self) -> &Path {
        self.0.parent().unwrap_or(&self.0)
    }

    /// A session file exists (the computer was linked at some point).
    pub fn exists(&self) -> bool {
        std::fs::metadata(&self.0).map(|m| m.len() > 0).unwrap_or(false)
    }

    /// Remove the session after a logout so the next start pairs afresh.
    pub fn remove(&self) -> std::io::Result<()> {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let f = PathBuf::from(format!("{}{suffix}", self.0.display()));
            if f.exists() {
                std::fs::remove_file(f)?;
            }
        }
        Ok(())
    }

    /// Copy the session to `dest_dir` with SQLite's online backup, through a
    /// separate read-only connection. Anyone holding the copy can read and
    /// send this shop's WhatsApp messages; callers must have the owner's
    /// explicit acknowledgement.
    pub fn backup_to(&self, dest_dir: &Path) -> AppResult<PathBuf> {
        if !self.exists() {
            return Err(AppError::not_found("WhatsApp session"));
        }
        std::fs::create_dir_all(dest_dir)?;
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let dest = dest_dir.join(format!("whatsapp-session-{ts}.db"));
        let src = rusqlite::Connection::open_with_flags(&self.0, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| AppError::internal(format!("Could not open the WhatsApp session: {e}")))?;
        src.backup(rusqlite::MAIN_DB, &dest, None).map_err(|e| AppError::internal(format!("WhatsApp session backup failed: {e}")))?;
        Ok(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_is_its_own_file_under_the_data_folder() {
        let d = tempfile::tempdir().unwrap();
        let s = SessionPath::for_data_dir(d.path()).unwrap();
        assert_eq!(s.path(), d.path().join("whatsapp").join("session.db"));
        assert_ne!(s.path(), d.path().join(amwapos_core::service::DB_FILE));
        let ledger = d.path().join("whatsapp").join("session.db");
        assert!(SessionPath::check(s.path(), &ledger).is_err(), "sharing the ledger file is refused");
        assert!(SessionPath::check(Path::new("data/session.db"), &ledger).is_err(), "relative paths are refused");
        let exe_dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        assert!(SessionPath::check(&exe_dir.join("session.db"), &ledger).is_err(), "next to the program is refused");
    }
}
