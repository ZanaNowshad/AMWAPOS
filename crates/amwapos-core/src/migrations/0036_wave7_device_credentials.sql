-- Wave 7: per-device credential rotation and revocation
-- (docs/OPERATIONAL_CONTROL.md).
--
-- A terminal's hub credential is derived from the hub's master secret (kept
-- in the operating system's credential store) and the credential VERSION.
-- These columns hold versions and times only: no key, no hash of a key,
-- nothing secret. They are hub-local and never replicated.
--
-- Every existing terminal keeps its current credential, as version 1.
ALTER TABLE devices ADD COLUMN credential_version INTEGER NOT NULL DEFAULT 1;
-- A rotation in progress: the version handed to the terminal, not yet
-- proven by a request signed with it.
ALTER TABLE devices ADD COLUMN credential_next_version INTEGER;
ALTER TABLE devices ADD COLUMN credential_staged_at TEXT;
-- After the terminal proves the new version, the previous one is accepted
-- for a short, bounded grace (requests already on their way), then never.
ALTER TABLE devices ADD COLUMN credential_prev_version INTEGER;
ALTER TABLE devices ADD COLUMN credential_grace_until TEXT;
ALTER TABLE devices ADD COLUMN credential_rotated_at TEXT;
-- Set when a terminal is revoked as lost or stolen: its credential version
-- was moved on, so re-activating it cannot bring the old credential back.
ALTER TABLE devices ADD COLUMN revocation_reason TEXT;
