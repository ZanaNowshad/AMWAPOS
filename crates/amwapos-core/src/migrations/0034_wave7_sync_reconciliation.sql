-- Wave 7: the Sync Reconciliation Centre (docs/OPERATIONAL_CONTROL.md).
--
-- `sync_dead_letters` stays the one record of changes that could not be
-- saved. It gains:
-- * `reason_code`: why, as a fixed code the screens explain in plain words.
--   Rows from before this upgrade keep their text and become
--   'legacy_unclassified' (nothing is guessed from old messages);
-- * `retryable`: whether trying again can succeed without changing the data;
-- * a third state, 'closed': a person decided not to apply the record. It
--   is kept, with who, when and why. A row is never deleted and its record
--   is never edited by hand;
-- * `resolution`, `resolution_note`, `resolved_by`, `resolved_at`: how it
--   ended. Rows resolved before this upgrade keep NULL here (the old version
--   did not record how).

CREATE TABLE sync_dead_letters_new (
  dead_id         TEXT PRIMARY KEY,
  direction       TEXT NOT NULL CHECK (direction IN ('push','pull','apply')),
  origin          TEXT,
  table_name      TEXT NOT NULL,
  row_pk          TEXT NOT NULL,
  op              TEXT NOT NULL,
  payload_json    TEXT,
  error           TEXT NOT NULL,
  attempts        INTEGER NOT NULL DEFAULT 1,
  status          TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','closed')),
  created_at      TEXT NOT NULL,
  last_attempt_at TEXT NOT NULL,
  reason_code     TEXT NOT NULL DEFAULT 'legacy_unclassified' CHECK (reason_code IN ('version_mismatch','not_permitted',
                    'invalid_record','missing_dependency','conflict','superseded','storage_error','unknown','legacy_unclassified')),
  retryable       INTEGER NOT NULL DEFAULT 1 CHECK (retryable IN (0,1)),
  resolution      TEXT CHECK (resolution IS NULL OR resolution IN ('applied','recovered','settled_on_hub','closed_without_applying')),
  resolution_note TEXT,
  resolved_by     TEXT,
  resolved_at     TEXT,
  retry_requested_at TEXT
);
INSERT INTO sync_dead_letters_new(dead_id, direction, origin, table_name, row_pk, op, payload_json, error, attempts, status, created_at,
    last_attempt_at, reason_code, retryable)
  SELECT dead_id, direction, origin, table_name, row_pk, op, payload_json, error, attempts, status, created_at, last_attempt_at,
    'legacy_unclassified', 1
  FROM sync_dead_letters ORDER BY rowid;
CREATE TEMP TABLE _dead_copy_check (ok INTEGER NOT NULL CHECK (ok = 1));
INSERT INTO _dead_copy_check(ok) SELECT
  (SELECT COUNT(*) FROM sync_dead_letters) = (SELECT COUNT(*) FROM sync_dead_letters_new)
  AND (SELECT COUNT(*) FROM sync_dead_letters o JOIN sync_dead_letters_new n ON n.dead_id=o.dead_id AND n.status=o.status
       AND n.error=o.error AND n.attempts=o.attempts AND COALESCE(n.payload_json,'')=COALESCE(o.payload_json,'')) = (SELECT COUNT(*) FROM sync_dead_letters);
DROP TABLE _dead_copy_check;
DROP TABLE sync_dead_letters;
ALTER TABLE sync_dead_letters_new RENAME TO sync_dead_letters;

CREATE INDEX ix_dead_open ON sync_dead_letters(status, origin, reason_code);
CREATE INDEX ix_dead_key ON sync_dead_letters(direction, table_name, row_pk) WHERE status = 'open';
CREATE INDEX ix_dead_created ON sync_dead_letters(created_at);

-- What a record is never changes; a finished row never changes; nothing is
-- deleted.
CREATE TRIGGER trg_dead_identity_fixed BEFORE UPDATE OF dead_id, direction, origin, table_name, row_pk, op, created_at ON sync_dead_letters
BEGIN SELECT RAISE(ABORT, 'a sync problem record is permanent'); END;
CREATE TRIGGER trg_dead_finished_final BEFORE UPDATE ON sync_dead_letters WHEN OLD.status <> 'open'
BEGIN SELECT RAISE(ABORT, 'a finished sync problem cannot change'); END;
CREATE TRIGGER trg_dead_no_delete BEFORE DELETE ON sync_dead_letters
BEGIN SELECT RAISE(ABORT, 'sync problem records are kept'); END;
