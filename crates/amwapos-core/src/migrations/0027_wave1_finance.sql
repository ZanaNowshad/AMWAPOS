-- Wave 1 of the merchant operating system (docs/MERCHANT_OS_PLAN.md).

-- The receipt exactly as issued: the rendered document (header, lines,
-- totals, footer) as canonical JSON, written in the same transaction as the
-- sale or refund, with its SHA-256. Reprints come from here, so later changes
-- to the business name, VAT number, address, header, footer or receipt
-- language never change an issued receipt. Records made before this
-- migration have no snapshot and reprint as reconstructed.
CREATE TABLE receipt_snapshots (
  ref_kind       TEXT NOT NULL CHECK (ref_kind IN ('sale','refund','void')),
  ref_id         TEXT NOT NULL,
  format_version INTEGER NOT NULL,
  doc_json       TEXT NOT NULL,
  copy_at        INTEGER NOT NULL,
  sha256         TEXT NOT NULL,
  created_at     TEXT NOT NULL,
  PRIMARY KEY (ref_kind, ref_id)
);
CREATE TRIGGER trg_receipt_snapshots_no_update BEFORE UPDATE ON receipt_snapshots BEGIN SELECT RAISE(ABORT, 'issued receipts are immutable'); END;
CREATE TRIGGER trg_receipt_snapshots_no_delete BEFORE DELETE ON receipt_snapshots BEGIN SELECT RAISE(ABORT, 'issued receipts are immutable'); END;
CREATE TRIGGER trg_sync_receipt_snapshots_insert AFTER INSERT ON receipt_snapshots WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('receipt_snapshots', json_object('ref_kind', NEW.ref_kind, 'ref_id', NEW.ref_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- Stock movements: rebuilt once (the type list is a CHECK) for the new
-- movement kinds of the merchant operating system — `void` (a completed sale
-- reversed the same day), `waste` and `supplier_return` — and an optional
-- `lot_id` for batches. Rows, order and totals are copied exactly; the
-- migration aborts if the count or the quantity total differs.
CREATE TABLE stock_movements_new (
  movement_id     TEXT PRIMARY KEY,
  product_id      TEXT NOT NULL REFERENCES products(product_id),
  branch_id       TEXT NOT NULL,
  type            TEXT NOT NULL CHECK (type IN ('sale','refund','receive','adjust','stocktake','opening','transfer_in','transfer_out','void','waste','supplier_return')),
  qty_delta_milli INTEGER NOT NULL,
  unit_cost_minor INTEGER,
  balance_after_milli INTEGER NOT NULL,
  source_type     TEXT NOT NULL,
  source_id       TEXT,
  reason          TEXT,
  user_id         TEXT,
  device_id       TEXT,
  created_at      TEXT NOT NULL,
  location_id     TEXT,
  lot_id          TEXT
);
INSERT INTO stock_movements_new(movement_id, product_id, branch_id, type, qty_delta_milli, unit_cost_minor, balance_after_milli,
    source_type, source_id, reason, user_id, device_id, created_at, location_id)
  SELECT movement_id, product_id, branch_id, type, qty_delta_milli, unit_cost_minor, balance_after_milli,
    source_type, source_id, reason, user_id, device_id, created_at, location_id
  FROM stock_movements ORDER BY rowid;
CREATE TEMP TABLE _movement_copy_check (ok INTEGER NOT NULL CHECK (ok = 1));
INSERT INTO _movement_copy_check(ok) SELECT
  (SELECT COUNT(*) FROM stock_movements) = (SELECT COUNT(*) FROM stock_movements_new)
  AND (SELECT COALESCE(SUM(qty_delta_milli),0) FROM stock_movements) = (SELECT COALESCE(SUM(qty_delta_milli),0) FROM stock_movements_new);
DROP TABLE _movement_copy_check;
DROP TABLE stock_movements;
ALTER TABLE stock_movements_new RENAME TO stock_movements;
CREATE INDEX ix_movements_product ON stock_movements(product_id, created_at);
CREATE INDEX ix_movements_created ON stock_movements(created_at);
CREATE INDEX ix_movements_source ON stock_movements(source_type, source_id);
CREATE INDEX ix_movements_lot ON stock_movements(lot_id) WHERE lot_id IS NOT NULL;
CREATE TRIGGER trg_moves_no_update BEFORE UPDATE ON stock_movements BEGIN SELECT RAISE(ABORT, 'stock movements are immutable'); END;
CREATE TRIGGER trg_moves_no_delete BEFORE DELETE ON stock_movements BEGIN SELECT RAISE(ABORT, 'stock movements are immutable'); END;
CREATE TRIGGER trg_sync_stock_movements_insert AFTER INSERT ON stock_movements WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('stock_movements', json_object('movement_id', NEW.movement_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- Completed-sale void. A void is its own record (`sale_voids`): the whole
-- sale cancelled the same trading day, before its shift closes, with a reason
-- and, for a cashier, a manager approval bound to that sale. The original
-- sale is never changed. The money, VAT, stock, customer-account and loyalty
-- reversal is written by the same code as a refund, into `refunds` with
-- kind 'void', so every net figure and the drawer count include it; reports
-- show voids separately.
ALTER TABLE refunds ADD COLUMN kind TEXT NOT NULL DEFAULT 'refund' CHECK (kind IN ('refund','void'));
CREATE TABLE sale_voids (
  void_id       TEXT PRIMARY KEY,
  sale_id       TEXT NOT NULL UNIQUE REFERENCES sales(sale_id),
  refund_id     TEXT NOT NULL UNIQUE REFERENCES refunds(refund_id),
  reason        TEXT NOT NULL,
  user_id       TEXT NOT NULL,
  approved_by   TEXT,
  branch_id     TEXT NOT NULL,
  device_id     TEXT NOT NULL,
  business_date TEXT NOT NULL,
  operation_id  TEXT NOT NULL UNIQUE,
  created_at    TEXT NOT NULL
);
CREATE TRIGGER trg_sale_voids_no_update BEFORE UPDATE ON sale_voids BEGIN SELECT RAISE(ABORT, 'voids are immutable'); END;
CREATE TRIGGER trg_sale_voids_no_delete BEFORE DELETE ON sale_voids BEGIN SELECT RAISE(ABORT, 'voids are immutable'); END;
CREATE TRIGGER trg_sync_sale_voids_insert AFTER INSERT ON sale_voids WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('sale_voids', json_object('void_id', NEW.void_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
