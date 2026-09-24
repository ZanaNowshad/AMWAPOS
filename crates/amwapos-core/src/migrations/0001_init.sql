-- AMWAPOS schema v1.
-- Conventions:
--   * IDs are ULID text (26 chars).
--   * Money columns end in _minor and are INTEGER minor units.
--   * Quantity columns end in _milli and are INTEGER thousandths.
--   * Timestamps are UTC RFC3339 with milliseconds ("2026-09-24T16:42:00.123Z").
--   * Financial and ledger tables are append-only; triggers enforce it.
-- This file is immutable once released. Add new migrations instead.

CREATE TABLE business (
  business_id   TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  name_ar       TEXT,
  cr_number     TEXT,
  vat_number    TEXT,
  phone         TEXT,
  address       TEXT,
  currency      TEXT NOT NULL DEFAULT 'BHD',
  currency_digits INTEGER NOT NULL DEFAULT 3 CHECK (currency_digits BETWEEN 0 AND 4),
  timezone      TEXT NOT NULL DEFAULT 'Asia/Bahrain',
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL
);

CREATE TABLE branches (
  branch_id   TEXT PRIMARY KEY,
  code        TEXT NOT NULL UNIQUE,
  name        TEXT NOT NULL,
  address     TEXT,
  phone       TEXT,
  cr_number   TEXT,
  vat_number  TEXT,
  currency    TEXT NOT NULL DEFAULT 'BHD',
  timezone    TEXT NOT NULL DEFAULT 'Asia/Bahrain',
  active      INTEGER NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

CREATE TABLE devices (
  device_id      TEXT PRIMARY KEY,
  branch_id      TEXT NOT NULL REFERENCES branches(branch_id),
  name           TEXT NOT NULL,
  device_code    TEXT NOT NULL UNIQUE,
  operating_mode TEXT NOT NULL CHECK (operating_mode IN ('standalone','hub','terminal')),
  active         INTEGER NOT NULL DEFAULT 1,
  activated_at   TEXT NOT NULL,
  revoked_at     TEXT,
  app_version    TEXT,
  os_info        TEXT,
  -- SHA-256 of the device's hub credential (hub side only). Never the credential itself.
  credential_hash TEXT
);

CREATE TABLE settings (
  key         TEXT PRIMARY KEY,
  value_json  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  updated_by  TEXT
);

CREATE TABLE roles (
  role_id     TEXT PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE,
  description TEXT,
  is_system   INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

CREATE TABLE permissions (
  code        TEXT PRIMARY KEY,
  domain      TEXT NOT NULL,
  description TEXT NOT NULL
);

CREATE TABLE role_permissions (
  role_id         TEXT NOT NULL REFERENCES roles(role_id) ON DELETE CASCADE,
  permission_code TEXT NOT NULL REFERENCES permissions(code),
  PRIMARY KEY (role_id, permission_code)
);

CREATE TABLE users (
  user_id         TEXT PRIMARY KEY,
  branch_id       TEXT REFERENCES branches(branch_id),
  display_name    TEXT NOT NULL,
  pin_hash        TEXT NOT NULL,
  role_id         TEXT NOT NULL REFERENCES roles(role_id),
  active          INTEGER NOT NULL DEFAULT 1,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);

-- Device-local login state (lockout counters are not replicated).
CREATE TABLE user_login_state (
  user_id         TEXT PRIMARY KEY REFERENCES users(user_id),
  failed_attempts INTEGER NOT NULL DEFAULT 0,
  locked_until    TEXT,
  last_login_at   TEXT,
  last_failed_at  TEXT
);

CREATE TABLE categories (
  category_id TEXT PRIMARY KEY,
  parent_id   TEXT REFERENCES categories(category_id),
  name        TEXT NOT NULL,
  sort_order  INTEGER NOT NULL DEFAULT 0,
  active      INTEGER NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX ux_categories_name ON categories(COALESCE(parent_id,''), name COLLATE NOCASE);

CREATE TABLE tax_rules (
  tax_rule_id    TEXT PRIMARY KEY,
  name           TEXT NOT NULL,
  rate_bp        INTEGER NOT NULL CHECK (rate_bp BETWEEN 0 AND 10000),
  inclusive      INTEGER NOT NULL DEFAULT 1,
  active         INTEGER NOT NULL DEFAULT 1,
  effective_from TEXT NOT NULL,
  effective_to   TEXT,
  created_at     TEXT NOT NULL
);

CREATE TABLE products (
  product_id            TEXT PRIMARY KEY,
  sku                   TEXT NOT NULL,
  name                  TEXT NOT NULL,
  name_ar               TEXT,
  description           TEXT,
  category_id           TEXT REFERENCES categories(category_id),
  tax_rule_id           TEXT NOT NULL REFERENCES tax_rules(tax_rule_id),
  unit                  TEXT NOT NULL DEFAULT 'pcs',
  track_inventory       INTEGER NOT NULL DEFAULT 1,
  allow_decimal_quantity INTEGER NOT NULL DEFAULT 0,
  reorder_point_milli   INTEGER NOT NULL DEFAULT 0,
  active                INTEGER NOT NULL DEFAULT 1,
  is_favorite           INTEGER NOT NULL DEFAULT 0,
  created_at            TEXT NOT NULL,
  updated_at            TEXT NOT NULL,
  archived_at           TEXT,
  version               INTEGER NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX ux_products_sku ON products(sku COLLATE NOCASE);
CREATE INDEX ix_products_category ON products(category_id);
CREATE INDEX ix_products_name ON products(name COLLATE NOCASE);

CREATE TABLE product_barcodes (
  barcode_id  TEXT PRIMARY KEY,
  product_id  TEXT NOT NULL REFERENCES products(product_id),
  barcode     TEXT NOT NULL UNIQUE,
  is_primary  INTEGER NOT NULL DEFAULT 0,
  source      TEXT NOT NULL DEFAULT 'manual',
  created_at  TEXT NOT NULL,
  created_by  TEXT
);
CREATE INDEX ix_barcodes_product ON product_barcodes(product_id);

CREATE TABLE product_prices (
  price_id       TEXT PRIMARY KEY,
  product_id     TEXT NOT NULL REFERENCES products(product_id),
  branch_id      TEXT,
  price_type     TEXT NOT NULL DEFAULT 'retail',
  amount_minor   INTEGER NOT NULL CHECK (amount_minor >= 0),
  effective_from TEXT NOT NULL,
  effective_to   TEXT,
  reason         TEXT,
  created_by     TEXT,
  created_at     TEXT NOT NULL
);
CREATE INDEX ix_prices_product ON product_prices(product_id, price_type, effective_from);

CREATE TABLE product_cost_history (
  cost_id      TEXT PRIMARY KEY,
  product_id   TEXT NOT NULL REFERENCES products(product_id),
  supplier_id  TEXT,
  cost_minor   INTEGER NOT NULL CHECK (cost_minor >= 0),
  source       TEXT NOT NULL,
  source_id    TEXT,
  effective_at TEXT NOT NULL,
  created_by   TEXT
);
CREATE INDEX ix_cost_product ON product_cost_history(product_id, effective_at);

-- Full-text search over name/sku/barcodes. Maintained by the application.
CREATE VIRTUAL TABLE products_fts USING fts5(
  product_id UNINDEXED,
  name,
  sku,
  barcodes,
  tokenize = 'unicode61 remove_diacritics 2',
  prefix = '2 3 4'
);

CREATE TABLE stock_levels (
  product_id       TEXT NOT NULL REFERENCES products(product_id),
  branch_id        TEXT NOT NULL REFERENCES branches(branch_id),
  qty_milli        INTEGER NOT NULL DEFAULT 0,
  last_movement_at TEXT,
  updated_at       TEXT NOT NULL,
  PRIMARY KEY (product_id, branch_id)
);

-- Costing: moving weighted-average cost per product/branch, maintained on
-- receiving (the single explicit costing method in v1). last_cost_minor is the
-- latest supplier cost. Hub-authoritative in multi-terminal mode.
CREATE TABLE product_costs (
  product_id      TEXT NOT NULL REFERENCES products(product_id),
  branch_id       TEXT NOT NULL,
  avg_cost_minor  INTEGER NOT NULL DEFAULT 0 CHECK (avg_cost_minor >= 0),
  last_cost_minor INTEGER NOT NULL DEFAULT 0 CHECK (last_cost_minor >= 0),
  updated_at      TEXT NOT NULL,
  PRIMARY KEY (product_id, branch_id)
);

CREATE TABLE stock_movements (
  movement_id     TEXT PRIMARY KEY,
  product_id      TEXT NOT NULL REFERENCES products(product_id),
  branch_id       TEXT NOT NULL,
  type            TEXT NOT NULL CHECK (type IN ('sale','refund','receive','adjust','stocktake','opening','transfer_in','transfer_out')),
  qty_delta_milli INTEGER NOT NULL,
  unit_cost_minor INTEGER,
  balance_after_milli INTEGER NOT NULL,
  source_type     TEXT NOT NULL,
  source_id       TEXT,
  reason          TEXT,
  user_id         TEXT,
  device_id       TEXT,
  created_at      TEXT NOT NULL
);
CREATE INDEX ix_movements_product ON stock_movements(product_id, created_at);
CREATE INDEX ix_movements_created ON stock_movements(created_at);
CREATE INDEX ix_movements_source ON stock_movements(source_type, source_id);

CREATE TABLE unknown_barcodes (
  barcode           TEXT PRIMARY KEY,
  first_seen_at     TEXT NOT NULL,
  last_seen_at      TEXT NOT NULL,
  scan_count        INTEGER NOT NULL DEFAULT 1,
  last_device_id    TEXT,
  last_user_id      TEXT,
  status            TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','dismissed')),
  resolved_product_id TEXT,
  resolved_by       TEXT,
  resolved_at       TEXT
);

CREATE TABLE shifts (
  shift_id            TEXT PRIMARY KEY,
  shift_number        TEXT NOT NULL UNIQUE,
  user_id             TEXT NOT NULL REFERENCES users(user_id),
  branch_id           TEXT NOT NULL,
  device_id           TEXT NOT NULL,
  business_date       TEXT NOT NULL,
  opening_float_minor INTEGER NOT NULL CHECK (opening_float_minor >= 0),
  opened_at           TEXT NOT NULL,
  closed_at           TEXT,
  status              TEXT NOT NULL CHECK (status IN ('open','closed')),
  expected_cash_minor INTEGER,
  counted_cash_minor  INTEGER,
  variance_minor      INTEGER,
  closed_by           TEXT,
  variance_approved_by TEXT,
  close_note          TEXT
);
-- At most one open shift per device.
CREATE UNIQUE INDEX ux_shift_open_device ON shifts(device_id) WHERE status = 'open';
CREATE INDEX ix_shifts_opened ON shifts(opened_at);

CREATE TABLE cash_events (
  cash_event_id TEXT PRIMARY KEY,
  shift_id      TEXT NOT NULL REFERENCES shifts(shift_id),
  type          TEXT NOT NULL CHECK (type IN ('paid_in','paid_out','safe_drop','no_sale')),
  amount_minor  INTEGER NOT NULL CHECK (amount_minor >= 0),
  reason        TEXT NOT NULL,
  actor_user_id TEXT NOT NULL,
  approved_by   TEXT,
  operation_id  TEXT NOT NULL UNIQUE,
  device_id     TEXT NOT NULL,
  created_at    TEXT NOT NULL
);
CREATE INDEX ix_cash_shift ON cash_events(shift_id);

CREATE TABLE customers (
  customer_id TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  phone       TEXT,
  whatsapp    TEXT,
  email       TEXT,
  area        TEXT,
  address     TEXT,
  active      INTEGER NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX ux_customers_phone ON customers(phone) WHERE phone IS NOT NULL AND phone <> '';
CREATE INDEX ix_customers_name ON customers(name COLLATE NOCASE);

CREATE TABLE customer_notes (
  note_id     TEXT PRIMARY KEY,
  customer_id TEXT NOT NULL REFERENCES customers(customer_id),
  author_id   TEXT NOT NULL,
  note        TEXT NOT NULL,
  created_at  TEXT NOT NULL
);

-- Working carts. Active and held carts live here so that an application
-- crash never loses a basket. A completed cart is linked to exactly one sale.
CREATE TABLE carts (
  cart_id             TEXT PRIMARY KEY,
  device_id           TEXT NOT NULL,
  branch_id           TEXT NOT NULL,
  user_id             TEXT NOT NULL,
  shift_id            TEXT,
  customer_id         TEXT REFERENCES customers(customer_id),
  status              TEXT NOT NULL CHECK (status IN ('active','held','completed','cancelled')),
  cart_discount_minor INTEGER NOT NULL DEFAULT 0 CHECK (cart_discount_minor >= 0),
  cart_discount_bp    INTEGER NOT NULL DEFAULT 0 CHECK (cart_discount_bp BETWEEN 0 AND 10000),
  cart_discount_approved_by TEXT,
  hold_number         INTEGER,
  hold_note           TEXT,
  held_at             TEXT,
  sale_id             TEXT,
  created_at          TEXT NOT NULL,
  updated_at          TEXT NOT NULL,
  version             INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX ix_carts_status ON carts(status, device_id);

CREATE TABLE cart_lines (
  line_id                   TEXT PRIMARY KEY,
  cart_id                   TEXT NOT NULL REFERENCES carts(cart_id) ON DELETE CASCADE,
  line_no                   INTEGER NOT NULL,
  product_id                TEXT REFERENCES products(product_id),
  name                      TEXT NOT NULL,
  sku                       TEXT,
  barcode                   TEXT,
  unit                      TEXT NOT NULL DEFAULT 'pcs',
  qty_milli                 INTEGER NOT NULL CHECK (qty_milli > 0),
  catalog_unit_price_minor  INTEGER NOT NULL CHECK (catalog_unit_price_minor >= 0),
  unit_price_minor          INTEGER NOT NULL CHECK (unit_price_minor >= 0),
  price_override_by         TEXT,
  line_discount_minor       INTEGER NOT NULL DEFAULT 0 CHECK (line_discount_minor >= 0),
  line_discount_bp          INTEGER NOT NULL DEFAULT 0 CHECK (line_discount_bp BETWEEN 0 AND 10000),
  discount_approved_by      TEXT,
  tax_rule_id               TEXT,
  tax_rate_bp               INTEGER NOT NULL,
  tax_inclusive             INTEGER NOT NULL,
  is_custom                 INTEGER NOT NULL DEFAULT 0,
  created_at                TEXT NOT NULL
);
CREATE INDEX ix_cart_lines_cart ON cart_lines(cart_id, line_no);

CREATE TABLE sales (
  sale_id          TEXT PRIMARY KEY,
  receipt_number   TEXT NOT NULL UNIQUE,
  branch_id        TEXT NOT NULL,
  device_id        TEXT NOT NULL,
  shift_id         TEXT NOT NULL,
  cashier_user_id  TEXT NOT NULL,
  customer_id      TEXT,
  status           TEXT NOT NULL CHECK (status IN ('completed')),
  subtotal_minor   INTEGER NOT NULL,
  discount_minor   INTEGER NOT NULL,
  tax_minor        INTEGER NOT NULL,
  total_minor      INTEGER NOT NULL,
  paid_minor       INTEGER NOT NULL,
  change_minor     INTEGER NOT NULL,
  cost_total_minor INTEGER NOT NULL,
  item_count_milli INTEGER NOT NULL,
  cart_id          TEXT,
  operation_id     TEXT NOT NULL UNIQUE,
  business_date    TEXT NOT NULL,
  completed_at     TEXT NOT NULL,
  created_at       TEXT NOT NULL,
  CHECK (total_minor >= 0 AND paid_minor - change_minor = total_minor AND change_minor >= 0)
);
CREATE INDEX ix_sales_completed ON sales(completed_at);
CREATE INDEX ix_sales_business_date ON sales(business_date);
CREATE INDEX ix_sales_shift ON sales(shift_id);
CREATE INDEX ix_sales_cashier ON sales(cashier_user_id, completed_at);
CREATE INDEX ix_sales_customer ON sales(customer_id, completed_at);

CREATE TABLE sale_items (
  sale_item_id                TEXT PRIMARY KEY,
  sale_id                     TEXT NOT NULL REFERENCES sales(sale_id),
  line_no                     INTEGER NOT NULL,
  product_id                  TEXT,
  product_name_snapshot       TEXT NOT NULL,
  sku_snapshot                TEXT,
  barcode_snapshot            TEXT,
  category_id_snapshot        TEXT,
  unit                        TEXT NOT NULL,
  qty_milli                   INTEGER NOT NULL CHECK (qty_milli > 0),
  original_unit_price_minor   INTEGER NOT NULL,
  effective_unit_price_minor  INTEGER NOT NULL,
  gross_minor                 INTEGER NOT NULL,
  discount_minor              INTEGER NOT NULL,
  tax_rule_id                 TEXT,
  tax_rate_bp                 INTEGER NOT NULL,
  tax_inclusive               INTEGER NOT NULL,
  tax_minor                   INTEGER NOT NULL,
  line_total_minor            INTEGER NOT NULL,
  cost_snapshot_minor         INTEGER NOT NULL,
  is_custom                   INTEGER NOT NULL DEFAULT 0,
  price_override_by           TEXT,
  discount_approved_by        TEXT
);
CREATE INDEX ix_sale_items_sale ON sale_items(sale_id);
CREATE INDEX ix_sale_items_product ON sale_items(product_id);

CREATE TABLE payments (
  payment_id     TEXT PRIMARY KEY,
  sale_id        TEXT NOT NULL REFERENCES sales(sale_id),
  method         TEXT NOT NULL,
  amount_minor   INTEGER NOT NULL CHECK (amount_minor > 0),
  tendered_minor INTEGER NOT NULL,
  change_minor   INTEGER NOT NULL DEFAULT 0,
  reference      TEXT,
  metadata_json  TEXT,
  created_at     TEXT NOT NULL
);
CREATE INDEX ix_payments_sale ON payments(sale_id);
CREATE INDEX ix_payments_method ON payments(method, created_at);

CREATE TABLE refunds (
  refund_id             TEXT PRIMARY KEY,
  refund_receipt_number TEXT NOT NULL UNIQUE,
  original_sale_id      TEXT NOT NULL REFERENCES sales(sale_id),
  branch_id             TEXT NOT NULL,
  device_id             TEXT NOT NULL,
  shift_id              TEXT NOT NULL,
  user_id               TEXT NOT NULL,
  approved_by           TEXT,
  reason                TEXT NOT NULL,
  subtotal_minor        INTEGER NOT NULL,
  tax_minor             INTEGER NOT NULL,
  total_minor           INTEGER NOT NULL CHECK (total_minor >= 0),
  cost_total_minor      INTEGER NOT NULL,
  operation_id          TEXT NOT NULL UNIQUE,
  business_date         TEXT NOT NULL,
  created_at            TEXT NOT NULL
);
CREATE INDEX ix_refunds_sale ON refunds(original_sale_id);
CREATE INDEX ix_refunds_created ON refunds(created_at);

CREATE TABLE refund_items (
  refund_item_id        TEXT PRIMARY KEY,
  refund_id             TEXT NOT NULL REFERENCES refunds(refund_id),
  original_sale_item_id TEXT NOT NULL REFERENCES sale_items(sale_item_id),
  product_id            TEXT,
  qty_milli             INTEGER NOT NULL CHECK (qty_milli > 0),
  amount_minor          INTEGER NOT NULL,
  tax_minor             INTEGER NOT NULL,
  cost_minor            INTEGER NOT NULL,
  restock               INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX ix_refund_items_orig ON refund_items(original_sale_item_id);

CREATE TABLE refund_tenders (
  refund_tender_id TEXT PRIMARY KEY,
  refund_id        TEXT NOT NULL REFERENCES refunds(refund_id),
  method           TEXT NOT NULL,
  amount_minor     INTEGER NOT NULL CHECK (amount_minor > 0),
  reference        TEXT
);

CREATE TABLE operation_idempotency (
  operation_id     TEXT PRIMARY KEY,
  operation_type   TEXT NOT NULL,
  actor_id         TEXT,
  device_id        TEXT,
  payload_hash     TEXT NOT NULL,
  status           TEXT NOT NULL CHECK (status IN ('completed')),
  result_reference TEXT,
  result_json      TEXT,
  created_at       TEXT NOT NULL,
  completed_at     TEXT
);

CREATE TABLE audit_logs (
  seq            INTEGER PRIMARY KEY AUTOINCREMENT,
  audit_id       TEXT NOT NULL UNIQUE,
  branch_id      TEXT,
  device_id      TEXT,
  user_id        TEXT,
  approved_by    TEXT,
  event_type     TEXT NOT NULL,
  entity_type    TEXT NOT NULL,
  entity_id      TEXT,
  before_json    TEXT,
  after_json     TEXT,
  previous_hash  TEXT NOT NULL,
  audit_hash     TEXT NOT NULL,
  app_version    TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  created_at     TEXT NOT NULL
);
CREATE INDEX ix_audit_entity ON audit_logs(entity_type, entity_id);
CREATE INDEX ix_audit_created ON audit_logs(created_at);
CREATE INDEX ix_audit_user ON audit_logs(user_id);

CREATE TABLE suppliers (
  supplier_id   TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  cr_number     TEXT,
  vat_number    TEXT,
  contact_name  TEXT,
  phone         TEXT,
  whatsapp      TEXT,
  email         TEXT,
  address       TEXT,
  payment_terms TEXT,
  notes         TEXT,
  active        INTEGER NOT NULL DEFAULT 1,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL
);
CREATE UNIQUE INDEX ux_suppliers_name ON suppliers(name COLLATE NOCASE);

CREATE TABLE purchase_orders (
  po_id          TEXT PRIMARY KEY,
  po_number      TEXT NOT NULL UNIQUE,
  supplier_id    TEXT NOT NULL REFERENCES suppliers(supplier_id),
  branch_id      TEXT NOT NULL,
  status         TEXT NOT NULL CHECK (status IN ('draft','ordered','partially_received','received','cancelled')),
  reference      TEXT,
  notes          TEXT,
  ordered_at     TEXT,
  expected_at    TEXT,
  subtotal_minor INTEGER NOT NULL DEFAULT 0,
  tax_minor      INTEGER NOT NULL DEFAULT 0,
  total_minor    INTEGER NOT NULL DEFAULT 0,
  created_by     TEXT NOT NULL,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL,
  version        INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX ix_po_supplier ON purchase_orders(supplier_id);
CREATE INDEX ix_po_status ON purchase_orders(status);

CREATE TABLE purchase_order_items (
  po_item_id           TEXT PRIMARY KEY,
  po_id                TEXT NOT NULL REFERENCES purchase_orders(po_id) ON DELETE CASCADE,
  line_no              INTEGER NOT NULL,
  product_id           TEXT NOT NULL REFERENCES products(product_id),
  qty_ordered_milli    INTEGER NOT NULL CHECK (qty_ordered_milli > 0),
  qty_received_milli   INTEGER NOT NULL DEFAULT 0 CHECK (qty_received_milli >= 0),
  unit_cost_minor      INTEGER NOT NULL CHECK (unit_cost_minor >= 0),
  tax_rate_bp          INTEGER NOT NULL DEFAULT 0,
  total_minor          INTEGER NOT NULL
);
CREATE INDEX ix_po_items_po ON purchase_order_items(po_id);

CREATE TABLE goods_receipts (
  receipt_id   TEXT PRIMARY KEY,
  po_id        TEXT REFERENCES purchase_orders(po_id),
  supplier_id  TEXT,
  branch_id    TEXT NOT NULL,
  reference    TEXT,
  total_cost_minor INTEGER NOT NULL,
  operation_id TEXT NOT NULL UNIQUE,
  user_id      TEXT NOT NULL,
  device_id    TEXT NOT NULL,
  created_at   TEXT NOT NULL
);

CREATE TABLE goods_receipt_items (
  receipt_item_id TEXT PRIMARY KEY,
  receipt_id      TEXT NOT NULL REFERENCES goods_receipts(receipt_id),
  po_item_id      TEXT,
  product_id      TEXT NOT NULL,
  qty_milli       INTEGER NOT NULL CHECK (qty_milli > 0),
  unit_cost_minor INTEGER NOT NULL CHECK (unit_cost_minor >= 0)
);

CREATE TABLE delivery_orders (
  delivery_id      TEXT PRIMARY KEY,
  delivery_number  TEXT NOT NULL UNIQUE,
  sale_id          TEXT REFERENCES sales(sale_id),
  customer_id      TEXT REFERENCES customers(customer_id),
  address          TEXT,
  area             TEXT,
  phone            TEXT,
  status           TEXT NOT NULL CHECK (status IN ('pending','preparing','dispatched','delivered','cancelled')),
  assigned_user_id TEXT,
  payment_status   TEXT NOT NULL CHECK (payment_status IN ('paid','pending','cod')),
  amount_minor     INTEGER NOT NULL DEFAULT 0,
  notes            TEXT,
  created_by       TEXT NOT NULL,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL,
  dispatched_at    TEXT,
  delivered_at     TEXT
);
CREATE INDEX ix_delivery_status ON delivery_orders(status);

CREATE TABLE delivery_events (
  event_id        TEXT PRIMARY KEY,
  delivery_id     TEXT NOT NULL REFERENCES delivery_orders(delivery_id),
  previous_status TEXT,
  new_status      TEXT NOT NULL,
  note            TEXT,
  user_id         TEXT NOT NULL,
  created_at      TEXT NOT NULL
);

CREATE TABLE stocktakes (
  stocktake_id   TEXT PRIMARY KEY,
  stocktake_number TEXT NOT NULL UNIQUE,
  name           TEXT NOT NULL,
  branch_id      TEXT NOT NULL,
  scope_type     TEXT NOT NULL CHECK (scope_type IN ('all','category','products')),
  scope_ref      TEXT,
  status         TEXT NOT NULL CHECK (status IN ('counting','review','completed','cancelled')),
  blind          INTEGER NOT NULL DEFAULT 1,
  created_by     TEXT NOT NULL,
  created_at     TEXT NOT NULL,
  finalized_by   TEXT,
  finalized_at   TEXT
);

CREATE TABLE stocktake_lines (
  stocktake_id       TEXT NOT NULL REFERENCES stocktakes(stocktake_id),
  product_id         TEXT NOT NULL REFERENCES products(product_id),
  -- System quantity when the stocktake was created (display only).
  expected_qty_milli INTEGER NOT NULL,
  counted_qty_milli  INTEGER,
  -- System quantity at the moment the line was counted. The adjustment is
  -- counted - system_qty_at_count, which stays correct if sales happen
  -- before or after the count.
  system_qty_at_count_milli INTEGER,
  unit_cost_minor    INTEGER NOT NULL DEFAULT 0,
  counted_by         TEXT,
  counted_at         TEXT,
  PRIMARY KEY (stocktake_id, product_id)
);

CREATE TABLE print_jobs (
  job_id      TEXT PRIMARY KEY,
  kind        TEXT NOT NULL,
  ref_type    TEXT,
  ref_id      TEXT,
  copy_label  TEXT,
  status      TEXT NOT NULL CHECK (status IN ('pending','printed','failed','cancelled')),
  attempts    INTEGER NOT NULL DEFAULT 0,
  last_error  TEXT,
  created_by  TEXT,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX ix_print_status ON print_jobs(status);

CREATE TABLE sequences (
  name  TEXT PRIMARY KEY,
  value INTEGER NOT NULL
);

CREATE TABLE backups (
  backup_id     TEXT PRIMARY KEY,
  path          TEXT NOT NULL,
  kind          TEXT NOT NULL CHECK (kind IN ('manual','automatic','safety')),
  size_bytes    INTEGER,
  sha256        TEXT,
  status        TEXT NOT NULL CHECK (status IN ('completed','failed')),
  error         TEXT,
  record_counts_json TEXT,
  duration_ms   INTEGER,
  created_by    TEXT,
  created_at    TEXT NOT NULL
);

-- Append-only guarantees for financial and ledger history.
CREATE TRIGGER trg_sales_no_update BEFORE UPDATE ON sales BEGIN SELECT RAISE(ABORT, 'sales are immutable'); END;
CREATE TRIGGER trg_sales_no_delete BEFORE DELETE ON sales BEGIN SELECT RAISE(ABORT, 'sales are immutable'); END;
CREATE TRIGGER trg_sale_items_no_update BEFORE UPDATE ON sale_items BEGIN SELECT RAISE(ABORT, 'sale items are immutable'); END;
CREATE TRIGGER trg_sale_items_no_delete BEFORE DELETE ON sale_items BEGIN SELECT RAISE(ABORT, 'sale items are immutable'); END;
CREATE TRIGGER trg_payments_no_update BEFORE UPDATE ON payments BEGIN SELECT RAISE(ABORT, 'payments are immutable'); END;
CREATE TRIGGER trg_payments_no_delete BEFORE DELETE ON payments BEGIN SELECT RAISE(ABORT, 'payments are immutable'); END;
CREATE TRIGGER trg_refunds_no_update BEFORE UPDATE ON refunds BEGIN SELECT RAISE(ABORT, 'refunds are immutable'); END;
CREATE TRIGGER trg_refunds_no_delete BEFORE DELETE ON refunds BEGIN SELECT RAISE(ABORT, 'refunds are immutable'); END;
CREATE TRIGGER trg_refund_items_no_update BEFORE UPDATE ON refund_items BEGIN SELECT RAISE(ABORT, 'refund items are immutable'); END;
CREATE TRIGGER trg_refund_items_no_delete BEFORE DELETE ON refund_items BEGIN SELECT RAISE(ABORT, 'refund items are immutable'); END;
CREATE TRIGGER trg_cash_no_update BEFORE UPDATE ON cash_events BEGIN SELECT RAISE(ABORT, 'cash events are immutable'); END;
CREATE TRIGGER trg_cash_no_delete BEFORE DELETE ON cash_events BEGIN SELECT RAISE(ABORT, 'cash events are immutable'); END;
CREATE TRIGGER trg_moves_no_update BEFORE UPDATE ON stock_movements BEGIN SELECT RAISE(ABORT, 'stock movements are immutable'); END;
CREATE TRIGGER trg_moves_no_delete BEFORE DELETE ON stock_movements BEGIN SELECT RAISE(ABORT, 'stock movements are immutable'); END;
CREATE TRIGGER trg_audit_no_update BEFORE UPDATE ON audit_logs BEGIN SELECT RAISE(ABORT, 'audit log is immutable'); END;
CREATE TRIGGER trg_audit_no_delete BEFORE DELETE ON audit_logs BEGIN SELECT RAISE(ABORT, 'audit log is immutable'); END;
CREATE TRIGGER trg_prices_no_delete BEFORE DELETE ON product_prices BEGIN SELECT RAISE(ABORT, 'price history is immutable'); END;
CREATE TRIGGER trg_costs_no_update BEFORE UPDATE ON product_cost_history BEGIN SELECT RAISE(ABORT, 'cost history is immutable'); END;
CREATE TRIGGER trg_costs_no_delete BEFORE DELETE ON product_cost_history BEGIN SELECT RAISE(ABORT, 'cost history is immutable'); END;
CREATE TRIGGER trg_idem_no_delete BEFORE DELETE ON operation_idempotency BEGIN SELECT RAISE(ABORT, 'operation log is immutable'); END;
