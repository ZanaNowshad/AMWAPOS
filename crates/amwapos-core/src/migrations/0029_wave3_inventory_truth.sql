-- Wave 3 of the merchant operating system: lots and expiry, waste
-- (docs/INVENTORY.md). Stock stays exactly one truth: `stock_movements`.
-- A lot holds no quantity of its own; its balance is worked out from the
-- movements tagged with it and, for sales that do not know their lot, a
-- first-expiring-first-out estimate made when it is read (never stored).
-- Nothing is invented for the past: no lots, expiry dates or waste are
-- created for existing stock, which shows as "not in a batch".

-- Per product: ask for batch and expiry at receiving, and what the date means.
ALTER TABLE products ADD COLUMN track_lots INTEGER NOT NULL DEFAULT 0;
ALTER TABLE products ADD COLUMN expiry_kind TEXT CHECK (expiry_kind IN ('use_by','best_before'));

-- A lot: stock received (or counted) together. The facts are permanent;
-- a correction is a `lot_corrections` row, and the newest one applies.
CREATE TABLE stock_lots (
  lot_id            TEXT PRIMARY KEY,
  lot_number        TEXT NOT NULL UNIQUE,
  product_id        TEXT NOT NULL REFERENCES products(product_id),
  branch_id         TEXT NOT NULL,
  location_id       TEXT,
  supplier_id       TEXT,
  po_id             TEXT,
  receipt_id        TEXT,
  supplier_lot_code TEXT,
  received_at       TEXT NOT NULL,
  manufactured_on   TEXT,
  expires_on        TEXT,
  expiry_kind       TEXT CHECK (expiry_kind IN ('use_by','best_before')),
  expiry_source     TEXT NOT NULL DEFAULT 'person' CHECK (expiry_source IN ('person','document')),
  qty_received_milli INTEGER NOT NULL CHECK (qty_received_milli > 0),
  unit_cost_minor   INTEGER NOT NULL CHECK (unit_cost_minor >= 0),
  provenance        TEXT NOT NULL CHECK (provenance IN ('receiving','count')),
  created_by        TEXT,
  created_at        TEXT NOT NULL
);
CREATE INDEX ix_stock_lots_product ON stock_lots(product_id, branch_id);
CREATE INDEX ix_stock_lots_expiry ON stock_lots(expires_on);
CREATE INDEX ix_movements_product_lotted ON stock_movements(product_id, branch_id, created_at, movement_id);
CREATE TRIGGER trg_stock_lots_no_update BEFORE UPDATE ON stock_lots BEGIN SELECT RAISE(ABORT, 'lot facts are permanent; record a correction'); END;
CREATE TRIGGER trg_stock_lots_no_delete BEFORE DELETE ON stock_lots BEGIN SELECT RAISE(ABORT, 'lots are kept'); END;

CREATE TABLE lot_corrections (
  correction_id     TEXT PRIMARY KEY,
  lot_id            TEXT NOT NULL REFERENCES stock_lots(lot_id),
  supplier_lot_code TEXT,
  manufactured_on   TEXT,
  expires_on        TEXT,
  expiry_kind       TEXT CHECK (expiry_kind IN ('use_by','best_before')),
  reason            TEXT NOT NULL,
  user_id           TEXT NOT NULL,
  operation_id      TEXT NOT NULL UNIQUE,
  created_at        TEXT NOT NULL
);
CREATE INDEX ix_lot_corrections_lot ON lot_corrections(lot_id, created_at);
CREATE TRIGGER trg_lot_corrections_no_update BEFORE UPDATE ON lot_corrections BEGIN SELECT RAISE(ABORT, 'lot history is permanent'); END;
CREATE TRIGGER trg_lot_corrections_no_delete BEFORE DELETE ON lot_corrections BEGIN SELECT RAISE(ABORT, 'lot history is permanent'); END;

-- Waste: stock that left without being sold, with its reason and cost.
-- Its stock movement (type 'waste') is the stock change; a mistake is
-- reversed by a compensating movement, and the record stays.
CREATE TABLE waste_records (
  waste_id             TEXT PRIMARY KEY,
  waste_number         TEXT NOT NULL UNIQUE,
  branch_id            TEXT NOT NULL,
  location_id          TEXT,
  product_id           TEXT NOT NULL REFERENCES products(product_id),
  lot_id               TEXT,
  qty_milli            INTEGER NOT NULL CHECK (qty_milli > 0),
  unit_cost_minor      INTEGER NOT NULL CHECK (unit_cost_minor >= 0),
  cost_minor           INTEGER NOT NULL CHECK (cost_minor >= 0),
  reason               TEXT NOT NULL CHECK (reason IN ('expired','damaged','spoiled','broken','shrinkage','internal_use','receiving_rejection','other')),
  business_date        TEXT NOT NULL,
  note                 TEXT,
  evidence_json        TEXT,
  user_id              TEXT NOT NULL,
  approved_by          TEXT,
  movement_id          TEXT NOT NULL,
  status               TEXT NOT NULL DEFAULT 'recorded' CHECK (status IN ('recorded','reversed')),
  reversed_by          TEXT,
  reversed_at          TEXT,
  reversal_reason      TEXT,
  reversal_movement_id TEXT,
  operation_id         TEXT NOT NULL UNIQUE,
  created_at           TEXT NOT NULL
);
CREATE INDEX ix_waste_date ON waste_records(branch_id, business_date);
CREATE INDEX ix_waste_product ON waste_records(product_id);
-- Only the reversal may be added, once; nothing else changes.
CREATE TRIGGER trg_waste_fixed BEFORE UPDATE ON waste_records
  WHEN OLD.status <> 'recorded' OR NEW.status <> 'reversed'
    OR NEW.waste_id IS NOT OLD.waste_id OR NEW.product_id IS NOT OLD.product_id OR NEW.lot_id IS NOT OLD.lot_id
    OR NEW.qty_milli IS NOT OLD.qty_milli OR NEW.cost_minor IS NOT OLD.cost_minor OR NEW.unit_cost_minor IS NOT OLD.unit_cost_minor
    OR NEW.reason IS NOT OLD.reason OR NEW.business_date IS NOT OLD.business_date OR NEW.movement_id IS NOT OLD.movement_id
    OR NEW.user_id IS NOT OLD.user_id OR NEW.approved_by IS NOT OLD.approved_by OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'waste records are permanent; reverse instead'); END;
CREATE TRIGGER trg_waste_no_delete BEFORE DELETE ON waste_records BEGIN SELECT RAISE(ABORT, 'waste records are permanent'); END;

-- Receiving drafts carry the batch and expiry. A date read from a supplier
-- document is a suggestion until a person confirms it.
ALTER TABLE receiving_draft_lines ADD COLUMN lot_code TEXT;
ALTER TABLE receiving_draft_lines ADD COLUMN expires_on TEXT;
ALTER TABLE receiving_draft_lines ADD COLUMN expiry_source TEXT CHECK (expiry_source IN ('person','document'));
ALTER TABLE receiving_draft_lines ADD COLUMN expiry_confirmed INTEGER NOT NULL DEFAULT 0;
