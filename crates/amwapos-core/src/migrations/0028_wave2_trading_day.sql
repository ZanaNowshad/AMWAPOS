-- Wave 2 of the merchant operating system: the trading day, registers and
-- drawers, the Z close and operational cases (docs/FINANCE.md, "Closing the
-- trading day"). The business date itself is the Wave 1 calculation
-- (`time::business_date`, stored on every sale, refund and shift); nothing
-- here recomputes it.

-- A register is the checkout station as a business concept. It is not the
-- Windows computer: a replaced computer can take over an existing register.
-- Every existing computer gets one register and one drawer with ids derived
-- from its device id, so every database (hub and terminals) creates the same
-- rows. Past shifts are NOT attributed to them: their register_id and
-- drawer_id stay NULL, because nobody recorded which register they used.
CREATE TABLE registers (
  register_id       TEXT PRIMARY KEY,
  branch_id         TEXT NOT NULL,
  code              TEXT NOT NULL UNIQUE,
  name              TEXT NOT NULL,
  active            INTEGER NOT NULL DEFAULT 1,
  device_id         TEXT,
  default_drawer_id TEXT,
  created_at        TEXT NOT NULL,
  updated_at        TEXT NOT NULL
);
CREATE UNIQUE INDEX ux_registers_device ON registers(device_id) WHERE device_id IS NOT NULL AND active = 1;

-- A drawer is the pool of cash a person is responsible for.
CREATE TABLE cash_drawers (
  drawer_id   TEXT PRIMARY KEY,
  branch_id   TEXT NOT NULL,
  register_id TEXT,
  name        TEXT NOT NULL,
  active      INTEGER NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

INSERT INTO cash_drawers(drawer_id, branch_id, register_id, name, active, created_at, updated_at)
  SELECT 'drw_' || device_id, branch_id, 'reg_' || device_id, 'Drawer ' || device_code, active,
         strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM devices;
INSERT INTO registers(register_id, branch_id, code, name, active, device_id, default_drawer_id, created_at, updated_at)
  SELECT 'reg_' || device_id, branch_id, device_code, name, active, CASE WHEN active = 1 THEN device_id END, 'drw_' || device_id,
         strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM devices;

-- A computer added later gets its register and drawer the same way.
CREATE TRIGGER trg_devices_register AFTER INSERT ON devices
BEGIN
  INSERT OR IGNORE INTO cash_drawers(drawer_id, branch_id, register_id, name, active, created_at, updated_at)
    VALUES ('drw_' || NEW.device_id, NEW.branch_id, 'reg_' || NEW.device_id, 'Drawer ' || NEW.device_code, 1,
            strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
  INSERT OR IGNORE INTO registers(register_id, branch_id, code, name, active, device_id, default_drawer_id, created_at, updated_at)
    SELECT 'reg_' || NEW.device_id, NEW.branch_id, NEW.device_code, NEW.name, 1,
           CASE WHEN NOT EXISTS (SELECT 1 FROM registers WHERE device_id = NEW.device_id AND active = 1) THEN NEW.device_id END,
           'drw_' || NEW.device_id, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now');
END;

-- The shift remains the cash session (who holds which cash, from open to
-- count). New shifts record their register and drawer.
ALTER TABLE shifts ADD COLUMN register_id TEXT;
ALTER TABLE shifts ADD COLUMN drawer_id TEXT;
CREATE INDEX ix_shifts_business_date ON shifts(branch_id, business_date);
CREATE INDEX ix_refunds_business_date ON refunds(branch_id, business_date);
CREATE INDEX ix_sales_branch_date ON sales(branch_id, business_date);

-- Registers and drawers are edited on the hub and copied to terminals.
CREATE TRIGGER trg_sync_registers_insert AFTER INSERT ON registers WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('registers', json_object('register_id', NEW.register_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_registers_update AFTER UPDATE ON registers WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('registers', json_object('register_id', NEW.register_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_cash_drawers_insert AFTER INSERT ON cash_drawers WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('cash_drawers', json_object('drawer_id', NEW.drawer_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_cash_drawers_update AFTER UPDATE ON cash_drawers WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('cash_drawers', json_object('drawer_id', NEW.drawer_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_registers_no_delete BEFORE DELETE ON registers BEGIN SELECT RAISE(ABORT, 'registers are deactivated, not deleted'); END;
CREATE TRIGGER trg_cash_drawers_no_delete BEFORE DELETE ON cash_drawers BEGIN SELECT RAISE(ABORT, 'drawers are deactivated, not deleted'); END;

-- The Z close: one permanent record per branch and trading day, made on the
-- hub. `snapshot_json` is the closed report exactly as closed (business and
-- branch names, cutoff, totals, VAT, tenders, drawers, after-close
-- adjustments) with its SHA-256, so later settings never change it.
CREATE TABLE day_closes (
  close_id        TEXT PRIMARY KEY,
  close_number    TEXT NOT NULL UNIQUE,
  branch_id       TEXT NOT NULL,
  business_date   TEXT NOT NULL,
  format_version  INTEGER NOT NULL,
  snapshot_json   TEXT NOT NULL,
  sha256          TEXT NOT NULL,
  sale_count      INTEGER NOT NULL,
  net_sales_minor INTEGER NOT NULL,
  tax_minor       INTEGER NOT NULL,
  late_count      INTEGER NOT NULL,
  variance_minor  INTEGER NOT NULL,
  closed_by       TEXT NOT NULL,
  closed_at       TEXT NOT NULL,
  operation_id    TEXT NOT NULL UNIQUE,
  UNIQUE (branch_id, business_date)
);
CREATE TRIGGER trg_day_closes_no_update BEFORE UPDATE ON day_closes BEGIN SELECT RAISE(ABORT, 'a closed trading day is permanent'); END;
CREATE TRIGGER trg_day_closes_no_delete BEFORE DELETE ON day_closes BEGIN SELECT RAISE(ABORT, 'a closed trading day is permanent'); END;

-- Which records each close counted. A record is counted by exactly one close
-- (primary key), so nothing is ever counted twice; a record that reaches the
-- hub after its own day was closed is counted by the next close, flagged
-- `late`, with its original business date kept.
CREATE TABLE day_close_items (
  ref_kind      TEXT NOT NULL CHECK (ref_kind IN ('sale','refund','shift')),
  ref_id        TEXT NOT NULL,
  close_id      TEXT NOT NULL REFERENCES day_closes(close_id),
  business_date TEXT NOT NULL,
  late          INTEGER NOT NULL CHECK (late IN (0,1)),
  PRIMARY KEY (ref_kind, ref_id)
);
CREATE INDEX ix_day_close_items_close ON day_close_items(close_id);
CREATE TRIGGER trg_day_close_items_no_update BEFORE UPDATE ON day_close_items BEGIN SELECT RAISE(ABORT, 'a closed trading day is permanent'); END;
CREATE TRIGGER trg_day_close_items_no_delete BEFORE DELETE ON day_close_items BEGIN SELECT RAISE(ABORT, 'a closed trading day is permanent'); END;

-- Operational cases: something a person should look into, with the facts,
-- a status and an unchangeable history. First use: cash differences.
CREATE TABLE cases (
  case_id          TEXT PRIMARY KEY,
  case_number      TEXT NOT NULL UNIQUE,
  kind             TEXT NOT NULL CHECK (kind IN ('cash_variance')),
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
  UNIQUE (kind, entity_type, entity_id)
);
CREATE INDEX ix_cases_status ON cases(status, branch_id);
-- The facts are fixed when the case opens; a finished case stays finished.
CREATE TRIGGER trg_cases_facts_fixed BEFORE UPDATE OF kind, branch_id, entity_type, entity_id, facts_json, created_at, created_by ON cases
BEGIN SELECT RAISE(ABORT, 'case facts are permanent'); END;
CREATE TRIGGER trg_cases_closed_final BEFORE UPDATE ON cases WHEN OLD.status IN ('resolved','dismissed')
BEGIN SELECT RAISE(ABORT, 'a finished case cannot change'); END;
CREATE TRIGGER trg_cases_no_delete BEFORE DELETE ON cases BEGIN SELECT RAISE(ABORT, 'cases are kept'); END;

CREATE TABLE case_events (
  event_id      TEXT PRIMARY KEY,
  case_id       TEXT NOT NULL REFERENCES cases(case_id),
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
CREATE TRIGGER trg_case_events_no_update BEFORE UPDATE ON case_events BEGIN SELECT RAISE(ABORT, 'case history is permanent'); END;
CREATE TRIGGER trg_case_events_no_delete BEFORE DELETE ON case_events BEGIN SELECT RAISE(ABORT, 'case history is permanent'); END;
