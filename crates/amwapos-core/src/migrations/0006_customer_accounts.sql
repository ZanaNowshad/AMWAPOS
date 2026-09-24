-- Customer accounts (feature: customer_credit) and saved addresses.
-- The ledger is append-only: a balance is the sum of its entries, positive
-- meaning the customer owes the store. Corrections are new entries.

CREATE TABLE customer_addresses (
  address_id  TEXT PRIMARY KEY,
  customer_id TEXT NOT NULL REFERENCES customers(customer_id),
  label       TEXT NOT NULL,
  area        TEXT,
  address     TEXT NOT NULL,
  notes       TEXT,
  is_default  INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX ix_customer_addresses ON customer_addresses(customer_id);

CREATE TABLE customer_accounts (
  customer_id        TEXT PRIMARY KEY REFERENCES customers(customer_id),
  enabled            INTEGER NOT NULL DEFAULT 0,
  credit_limit_minor INTEGER NOT NULL DEFAULT 0 CHECK (credit_limit_minor >= 0),
  updated_by         TEXT,
  updated_at         TEXT NOT NULL
);

CREATE TABLE customer_ledger (
  entry_id     TEXT PRIMARY KEY,
  customer_id  TEXT NOT NULL REFERENCES customers(customer_id),
  kind         TEXT NOT NULL CHECK (kind IN ('sale','payment','refund','adjustment')),
  amount_minor INTEGER NOT NULL CHECK (amount_minor <> 0),
  ref_type     TEXT,
  ref_id       TEXT,
  method       TEXT,
  note         TEXT,
  operation_id TEXT NOT NULL UNIQUE,
  user_id      TEXT NOT NULL,
  device_id    TEXT NOT NULL,
  created_at   TEXT NOT NULL
);
CREATE INDEX ix_customer_ledger ON customer_ledger(customer_id, created_at);
CREATE TRIGGER trg_customer_ledger_no_update BEFORE UPDATE ON customer_ledger BEGIN SELECT RAISE(ABORT, 'account entries are immutable'); END;
CREATE TRIGGER trg_customer_ledger_no_delete BEFORE DELETE ON customer_ledger BEGIN SELECT RAISE(ABORT, 'account entries are immutable'); END;

CREATE TRIGGER trg_sync_customer_addresses_insert AFTER INSERT ON customer_addresses WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customer_addresses', json_object('address_id', NEW.address_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_customer_addresses_update AFTER UPDATE ON customer_addresses WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customer_addresses', json_object('address_id', NEW.address_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_customer_addresses_delete AFTER DELETE ON customer_addresses WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customer_addresses', json_object('address_id', OLD.address_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_customer_accounts_insert AFTER INSERT ON customer_accounts WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customer_accounts', json_object('customer_id', NEW.customer_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_customer_accounts_update AFTER UPDATE ON customer_accounts WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customer_accounts', json_object('customer_id', NEW.customer_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_customer_accounts_delete AFTER DELETE ON customer_accounts WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customer_accounts', json_object('customer_id', OLD.customer_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_customer_ledger_insert AFTER INSERT ON customer_ledger WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customer_ledger', json_object('entry_id', NEW.entry_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
