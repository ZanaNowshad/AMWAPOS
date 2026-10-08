-- Wave 7 of the merchant operating system: the Alert Centre
-- (docs/OPERATIONAL_CONTROL.md).
--
-- The existing cases (Wave 2) stay the one place where a person handles an
-- operational problem. This migration widens them so the system can open a
-- case for a condition it measured (a till not seen, records that could not
-- be saved, an overdue backup), without a second alert engine:
-- * `source` says who opened the case: a person, the system, or the old AI
--   inbox (legacy);
-- * a system case carries a stable `dedupe_key`: at most one open case per
--   key (a unique index), so one incident is one case however often the
--   checks run;
-- * `first_seen_at`, `last_seen_at`, `occurrences`, `condition_active` and
--   `latest_json` follow the condition while the case is open; `facts_json`
--   stays what was seen when the case opened.
--
-- Existing cash-difference cases are copied exactly (the migration stops if
-- a row or an event is lost). Nothing is invented: no case is opened for
-- anything that happened before this upgrade. The old AI inbox rows are
-- copied as legacy cases with their own text and dismissal; the old table is
-- kept read-only for one release.

CREATE TABLE cases_new (
  case_id          TEXT PRIMARY KEY,
  case_number      TEXT NOT NULL UNIQUE,
  kind             TEXT NOT NULL CHECK (kind IN ('cash_variance','sync_failures','terminal_not_seen','terminal_backlog',
                     'terminal_incompatible','credential_rotation_stale','backup_overdue','print_failures',
                     'payment_review_backlog','rider_cash_held','legacy_alert')),
  severity         TEXT NOT NULL CHECK (severity IN ('low','medium','high')),
  status           TEXT NOT NULL CHECK (status IN ('new','acknowledged','in_progress','resolved','dismissed')),
  branch_id        TEXT NOT NULL,
  entity_type      TEXT NOT NULL,
  entity_id        TEXT NOT NULL,
  title            TEXT NOT NULL,
  facts_json       TEXT NOT NULL,
  assignee_user_id TEXT,
  created_by       TEXT,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL,
  resolution_code  TEXT,
  resolution_note  TEXT,
  resolved_by      TEXT,
  resolved_at      TEXT,
  source           TEXT NOT NULL DEFAULT 'user' CHECK (source IN ('user','system','legacy')),
  dedupe_key       TEXT,
  device_id        TEXT,
  link             TEXT,
  latest_json      TEXT,
  condition_active INTEGER CHECK (condition_active IS NULL OR condition_active IN (0,1)),
  first_seen_at    TEXT,
  last_seen_at     TEXT,
  occurrences      INTEGER NOT NULL DEFAULT 1,
  legacy_ref       TEXT
);
-- A cash case opened by the sweep has no person (created_by NULL): it was
-- the system. One opened by hand names the person.
INSERT INTO cases_new(case_id, case_number, kind, severity, status, branch_id, entity_type, entity_id, title, facts_json, assignee_user_id,
    created_by, created_at, updated_at, resolution_code, resolution_note, resolved_by, resolved_at, source, link)
  SELECT case_id, case_number, kind, severity, status, branch_id, entity_type, entity_id, title, facts_json, assignee_user_id,
    created_by, created_at, updated_at, resolution_code, resolution_note, resolved_by, resolved_at,
    CASE WHEN created_by IS NULL THEN 'system' ELSE 'user' END, '/admin/cases'
  FROM cases ORDER BY rowid;
CREATE TEMP TABLE _cases_copy_check (ok INTEGER NOT NULL CHECK (ok = 1));
INSERT INTO _cases_copy_check(ok) SELECT
  (SELECT COUNT(*) FROM cases) = (SELECT COUNT(*) FROM cases_new)
  AND (SELECT COUNT(*) FROM cases c JOIN cases_new n ON n.case_id=c.case_id AND n.status=c.status AND n.facts_json=c.facts_json
       AND n.severity=c.severity AND n.title=c.title AND n.case_number=c.case_number) = (SELECT COUNT(*) FROM cases);
DROP TABLE _cases_copy_check;
-- The case history is rebuilt beside it (same rows, same columns) so the
-- child table can be dropped before its parent and the renames below
-- re-point its foreign key at the new cases table.
CREATE TABLE case_events_new (
  event_id      TEXT PRIMARY KEY,
  case_id       TEXT NOT NULL REFERENCES cases_new(case_id),
  seq           INTEGER NOT NULL,
  kind          TEXT NOT NULL CHECK (kind IN ('created','status','note','assigned','evidence')),
  from_status   TEXT,
  to_status     TEXT,
  note          TEXT,
  evidence_json TEXT,
  user_id       TEXT,
  operation_id  TEXT UNIQUE,
  at            TEXT NOT NULL,
  UNIQUE (case_id, seq)
);
INSERT INTO case_events_new(event_id, case_id, seq, kind, from_status, to_status, note, evidence_json, user_id, operation_id, at)
  SELECT event_id, case_id, seq, kind, from_status, to_status, note, evidence_json, user_id, operation_id, at FROM case_events ORDER BY rowid;
CREATE TEMP TABLE _events_copy_check (ok INTEGER NOT NULL CHECK (ok = 1));
INSERT INTO _events_copy_check(ok) SELECT (SELECT COUNT(*) FROM case_events) = (SELECT COUNT(*) FROM case_events_new);
DROP TABLE _events_copy_check;
DROP TABLE case_events;
DROP TABLE cases;
ALTER TABLE cases_new RENAME TO cases;
ALTER TABLE case_events_new RENAME TO case_events;
CREATE TRIGGER trg_case_events_no_update BEFORE UPDATE ON case_events BEGIN SELECT RAISE(ABORT, 'case history is permanent'); END;
CREATE TRIGGER trg_case_events_no_delete BEFORE DELETE ON case_events BEGIN SELECT RAISE(ABORT, 'case history is permanent'); END;

CREATE UNIQUE INDEX ux_cases_cash ON cases(kind, entity_type, entity_id) WHERE kind = 'cash_variance';
CREATE UNIQUE INDEX ux_cases_legacy ON cases(legacy_ref) WHERE legacy_ref IS NOT NULL;
-- One open case per incident key.
CREATE UNIQUE INDEX ux_cases_open_key ON cases(dedupe_key) WHERE dedupe_key IS NOT NULL AND status NOT IN ('resolved','dismissed');
CREATE INDEX ix_cases_status ON cases(status, branch_id);
CREATE INDEX ix_cases_queue ON cases(status, severity, created_at);
CREATE INDEX ix_cases_key ON cases(dedupe_key, created_at) WHERE dedupe_key IS NOT NULL;
CREATE INDEX ix_cases_device ON cases(device_id) WHERE device_id IS NOT NULL;
CREATE TRIGGER trg_cases_facts_fixed BEFORE UPDATE OF kind, branch_id, entity_type, entity_id, facts_json, created_at, created_by,
    source, dedupe_key, device_id, legacy_ref ON cases
BEGIN SELECT RAISE(ABORT, 'case facts are permanent'); END;
CREATE TRIGGER trg_cases_closed_final BEFORE UPDATE ON cases WHEN OLD.status IN ('resolved','dismissed')
BEGIN SELECT RAISE(ABORT, 'a finished case cannot change'); END;
CREATE TRIGGER trg_cases_no_delete BEFORE DELETE ON cases BEGIN SELECT RAISE(ABORT, 'cases are kept'); END;

-- What the condition checks remember between runs (one row per incident
-- key): whether the condition holds, since when, and whether a person closed
-- its case while it still held (then no new case is opened until the
-- condition has cleared once). This is the checks' memory, not a second
-- alert list: every alert a person sees is a case.
CREATE TABLE alert_conditions (
  dedupe_key        TEXT PRIMARY KEY,
  kind              TEXT NOT NULL,
  active            INTEGER NOT NULL CHECK (active IN (0,1)),
  active_since      TEXT,
  cleared_since     TEXT,
  last_evaluated_at TEXT NOT NULL,
  suppressed        INTEGER NOT NULL DEFAULT 0 CHECK (suppressed IN (0,1)),
  last_case_id      TEXT
);

-- ---------------------------------------------------------------- legacy AI inbox
-- Each real row becomes a legacy case with its own words, time and
-- dismissal. Severity: info → low, warning → medium, danger → high.
INSERT INTO cases(case_id, case_number, kind, severity, status, branch_id, entity_type, entity_id, title, facts_json, created_by,
    created_at, updated_at, resolution_code, resolution_note, resolved_by, resolved_at, source, legacy_ref, link)
  SELECT 'legacy-' || a.alert_id,
    printf('L-%05d', ROW_NUMBER() OVER (ORDER BY a.created_at, a.alert_id)),
    'legacy_alert',
    CASE a.severity WHEN 'danger' THEN 'high' WHEN 'warning' THEN 'medium' ELSE 'low' END,
    CASE WHEN a.dismissed_at IS NULL THEN 'new' ELSE 'dismissed' END,
    COALESCE((SELECT branch_id FROM branches ORDER BY rowid LIMIT 1), ''),
    'ai_alert', a.alert_id, a.title,
    json_object('legacy_kind', a.kind, 'day', a.day_key, 'detail', json(a.detail_json), 'source', 'AI inbox (before the Alert Centre)'),
    NULL, a.created_at, COALESCE(a.dismissed_at, a.created_at),
    CASE WHEN a.dismissed_at IS NULL THEN NULL ELSE 'legacy_dismissed' END,
    CASE WHEN a.dismissed_at IS NULL THEN NULL ELSE 'Dismissed in the AI inbox before the Alert Centre existed.' END,
    a.dismissed_by, a.dismissed_at, 'legacy', a.alert_id, NULL
  FROM ai_alerts a ORDER BY a.created_at, a.alert_id;
INSERT INTO case_events(event_id, case_id, seq, kind, from_status, to_status, note, evidence_json, user_id, operation_id, at)
  SELECT 'legacy-' || alert_id || '-1', 'legacy-' || alert_id, 1, 'created', NULL, 'new', 'Copied from the AI inbox.', NULL, NULL, NULL, created_at
  FROM ai_alerts;
INSERT INTO case_events(event_id, case_id, seq, kind, from_status, to_status, note, evidence_json, user_id, operation_id, at)
  SELECT 'legacy-' || alert_id || '-2', 'legacy-' || alert_id, 2, 'status', 'new', 'dismissed', 'Dismissed in the AI inbox.', NULL, dismissed_by, NULL, dismissed_at
  FROM ai_alerts WHERE dismissed_at IS NOT NULL;
CREATE TEMP TABLE _legacy_copy_check (ok INTEGER NOT NULL CHECK (ok = 1));
INSERT INTO _legacy_copy_check(ok) SELECT (SELECT COUNT(*) FROM ai_alerts) = (SELECT COUNT(*) FROM cases WHERE source='legacy');
DROP TABLE _legacy_copy_check;

-- The old inbox is read-only from now on: nothing writes operational alerts
-- there any more.
CREATE TRIGGER trg_ai_alerts_read_only_insert BEFORE INSERT ON ai_alerts BEGIN SELECT RAISE(ABORT, 'the AI inbox is read-only; alerts are cases'); END;
CREATE TRIGGER trg_ai_alerts_read_only_update BEFORE UPDATE ON ai_alerts BEGIN SELECT RAISE(ABORT, 'the AI inbox is read-only; alerts are cases'); END;
CREATE TRIGGER trg_ai_alerts_read_only_delete BEFORE DELETE ON ai_alerts BEGIN SELECT RAISE(ABORT, 'the AI inbox is read-only; alerts are cases'); END;
