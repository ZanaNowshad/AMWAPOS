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

-- Operating expenses (back office, on the hub; not replicated, like
-- payables). An expense is drafted, submitted, approved (or rejected) and
-- paid; a mistake is voided, never edited or deleted after submission.
-- Amounts are fils: total = net + VAT. Operating profit uses `net_minor` of
-- approved and paid expenses by business date.
CREATE TABLE expense_categories (
  category_id TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  name_ar     TEXT,
  active      INTEGER NOT NULL DEFAULT 1,
  sort        INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL
);
INSERT INTO expense_categories(category_id, name, name_ar, sort, created_at) VALUES
  ('exp_rent','Rent','الإيجار',10,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_electricity','Electricity','الكهرباء',20,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_water','Water','المياه',30,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_internet','Internet and telephone','الإنترنت والهاتف',40,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_salaries','Salaries and wages','الرواتب والأجور',50,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_transport','Transport and petrol','النقل والوقود',60,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_delivery','Delivery','التوصيل',70,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_repairs','Repairs and maintenance','الإصلاح والصيانة',80,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_cleaning','Cleaning','التنظيف',90,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_supplies','Shop supplies','مستلزمات المحل',100,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_bank','Bank fees','الرسوم البنكية',110,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_government','Government fees and licences','الرسوم الحكومية والتراخيص',120,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_marketing','Marketing','التسويق',130,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_professional','Professional services','الخدمات المهنية',140,strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  ('exp_other','Other','أخرى',900,strftime('%Y-%m-%dT%H:%M:%fZ','now'));

CREATE TABLE expenses (
  expense_id     TEXT PRIMARY KEY,
  number         TEXT NOT NULL UNIQUE,
  branch_id      TEXT NOT NULL,
  business_date  TEXT NOT NULL,
  category_id    TEXT NOT NULL REFERENCES expense_categories(category_id),
  supplier_id    TEXT REFERENCES suppliers(supplier_id),
  payee          TEXT,
  description    TEXT NOT NULL,
  net_minor      INTEGER NOT NULL CHECK (net_minor >= 0),
  vat_minor      INTEGER NOT NULL DEFAULT 0 CHECK (vat_minor >= 0),
  total_minor    INTEGER NOT NULL CHECK (total_minor > 0 AND total_minor = net_minor + vat_minor),
  status         TEXT NOT NULL CHECK (status IN ('draft','submitted','approved','rejected','paid','void')),
  payment_method TEXT CHECK (payment_method IN ('petty_cash','till_paid_out','bank_transfer','card','cheque','other')),
  fund_id        TEXT,
  cash_event_id  TEXT UNIQUE,
  reference      TEXT,
  notes          TEXT,
  recurring_id   TEXT,
  created_by     TEXT NOT NULL,
  submitted_by   TEXT,
  submitted_at   TEXT,
  decided_by     TEXT,
  decided_at     TEXT,
  decision_note  TEXT,
  paid_by        TEXT,
  paid_at        TEXT,
  void_by        TEXT,
  void_at        TEXT,
  void_reason    TEXT,
  revision       INTEGER NOT NULL DEFAULT 1,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL,
  UNIQUE (recurring_id, business_date)
);
CREATE INDEX ix_expenses_date ON expenses(business_date, status);
CREATE INDEX ix_expenses_status ON expenses(status, created_at);
-- Money fields freeze once the expense leaves draft.
CREATE TRIGGER trg_expenses_freeze BEFORE UPDATE OF net_minor, vat_minor, total_minor, category_id, business_date, branch_id ON expenses
  WHEN OLD.status <> 'draft'
BEGIN SELECT RAISE(ABORT, 'a submitted expense cannot be edited; void it and enter it again'); END;
CREATE TRIGGER trg_expenses_no_delete BEFORE DELETE ON expenses WHEN OLD.status <> 'draft'
BEGIN SELECT RAISE(ABORT, 'only drafts can be deleted'); END;

CREATE TABLE expense_attachments (
  attachment_id TEXT PRIMARY KEY,
  expense_id    TEXT NOT NULL REFERENCES expenses(expense_id),
  file_name     TEXT NOT NULL,
  path          TEXT NOT NULL,
  mime          TEXT NOT NULL,
  sha256        TEXT NOT NULL,
  added_by      TEXT NOT NULL,
  added_at      TEXT NOT NULL
);
CREATE INDEX ix_expense_attachments ON expense_attachments(expense_id);

-- Recurring expenses produce drafts on their date; nothing is posted by itself.
CREATE TABLE expense_recurring (
  recurring_id  TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  category_id   TEXT NOT NULL REFERENCES expense_categories(category_id),
  supplier_id   TEXT,
  payee         TEXT,
  description   TEXT NOT NULL,
  net_minor     INTEGER NOT NULL CHECK (net_minor >= 0),
  vat_minor     INTEGER NOT NULL DEFAULT 0 CHECK (vat_minor >= 0),
  cadence       TEXT NOT NULL CHECK (cadence IN ('monthly','weekly')),
  day           INTEGER NOT NULL,
  next_date     TEXT NOT NULL,
  branch_id     TEXT NOT NULL,
  active        INTEGER NOT NULL DEFAULT 1,
  created_by    TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL
);

-- Petty cash: a fund kept apart from the till drawer, with a responsible
-- person. Its balance is the sum of its entries (never stored); a count
-- records what was found and the difference.
CREATE TABLE petty_cash_funds (
  fund_id     TEXT PRIMARY KEY,
  branch_id   TEXT NOT NULL,
  name        TEXT NOT NULL,
  custodian_user_id TEXT,
  active      INTEGER NOT NULL DEFAULT 1,
  created_by  TEXT NOT NULL,
  created_at  TEXT NOT NULL
);
CREATE TABLE petty_cash_entries (
  entry_id      TEXT PRIMARY KEY,
  fund_id       TEXT NOT NULL REFERENCES petty_cash_funds(fund_id),
  kind          TEXT NOT NULL CHECK (kind IN ('open','top_up','expense','reimburse','adjust','count','void')),
  amount_minor  INTEGER NOT NULL,
  counted_minor INTEGER,
  expense_id    TEXT REFERENCES expenses(expense_id),
  note          TEXT,
  operation_id  TEXT NOT NULL UNIQUE,
  user_id       TEXT NOT NULL,
  business_date TEXT NOT NULL,
  created_at    TEXT NOT NULL
);
CREATE INDEX ix_petty_entries ON petty_cash_entries(fund_id, created_at);
CREATE TRIGGER trg_petty_no_update BEFORE UPDATE ON petty_cash_entries BEGIN SELECT RAISE(ABORT, 'petty cash entries are immutable'); END;
CREATE TRIGGER trg_petty_no_delete BEFORE DELETE ON petty_cash_entries BEGIN SELECT RAISE(ABORT, 'petty cash entries are immutable'); END;

-- Customer credit terms: days after a charge before it counts as late
-- (ageing). Existing accounts get the usual 30 days.
ALTER TABLE customer_accounts ADD COLUMN terms_days INTEGER NOT NULL DEFAULT 30;
