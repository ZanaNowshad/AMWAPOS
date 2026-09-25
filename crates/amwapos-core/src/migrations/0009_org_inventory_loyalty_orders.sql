-- Product brief pillars 2, 4, 6, 8, 10, 11. Every table starts empty and
-- nothing reads it while its flag is off, so a store that keeps the flags off
-- behaves exactly as before. Existing branches get one default stockroom.

-- Pillar 2: stock locations inside a branch. The default location holds
-- everything not recorded at another location (branch total − others).
CREATE TABLE stock_locations (
  location_id TEXT PRIMARY KEY,
  branch_id   TEXT NOT NULL REFERENCES branches(branch_id),
  code        TEXT NOT NULL,
  name        TEXT NOT NULL,
  is_default  INTEGER NOT NULL DEFAULT 0,
  active      INTEGER NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  UNIQUE (branch_id, code)
);
INSERT INTO stock_locations(location_id, branch_id, code, name, is_default, active, created_at, updated_at)
  SELECT 'LOC-' || branch_id, branch_id, 'STOCKROOM', 'Stockroom', 1, 1, created_at, created_at FROM branches;

ALTER TABLE stock_movements ADD COLUMN location_id TEXT;

-- Transfers (between locations of one branch, or between branches of the
-- organisation). Shipping writes transfer_out movements at the source;
-- receiving writes transfer_in at the destination; shipped − received is in transit.
CREATE TABLE stock_transfers (
  transfer_id          TEXT PRIMARY KEY,
  transfer_number      TEXT NOT NULL UNIQUE,
  from_branch_id       TEXT NOT NULL REFERENCES branches(branch_id),
  from_location_id     TEXT NOT NULL REFERENCES stock_locations(location_id),
  to_branch_id         TEXT NOT NULL REFERENCES branches(branch_id),
  to_location_id       TEXT NOT NULL REFERENCES stock_locations(location_id),
  status               TEXT NOT NULL CHECK (status IN ('draft','shipped','received','cancelled')),
  note                 TEXT,
  ship_operation_id    TEXT UNIQUE,
  receive_operation_id TEXT UNIQUE,
  created_by           TEXT NOT NULL,
  created_at           TEXT NOT NULL,
  shipped_by           TEXT,
  shipped_at           TEXT,
  received_by          TEXT,
  received_at          TEXT,
  updated_at           TEXT NOT NULL
);
CREATE INDEX ix_stock_transfers_status ON stock_transfers(status, updated_at);
CREATE TABLE stock_transfer_lines (
  transfer_id        TEXT NOT NULL REFERENCES stock_transfers(transfer_id),
  line_no            INTEGER NOT NULL,
  product_id         TEXT NOT NULL REFERENCES products(product_id),
  qty_milli          INTEGER NOT NULL CHECK (qty_milli > 0),
  qty_received_milli INTEGER NOT NULL DEFAULT 0 CHECK (qty_received_milli >= 0),
  PRIMARY KEY (transfer_id, line_no)
);

-- Pillar 4: loyalty. An append-only points ledger; a balance is the sum.
CREATE TABLE loyalty_ledger (
  entry_id    TEXT PRIMARY KEY,
  customer_id TEXT NOT NULL REFERENCES customers(customer_id),
  kind        TEXT NOT NULL CHECK (kind IN ('earn','redeem','reverse_earn','reverse_redeem','adjust')),
  points      INTEGER NOT NULL CHECK (points <> 0),
  sale_id     TEXT,
  refund_id   TEXT,
  note        TEXT,
  user_id     TEXT,
  device_id   TEXT,
  created_at  TEXT NOT NULL
);
CREATE INDEX ix_loyalty_customer ON loyalty_ledger(customer_id, created_at);
CREATE INDEX ix_loyalty_sale ON loyalty_ledger(sale_id);
ALTER TABLE carts ADD COLUMN loyalty_points INTEGER NOT NULL DEFAULT 0 CHECK (loyalty_points >= 0);
ALTER TABLE sales ADD COLUMN loyalty_discount_minor INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sale_items ADD COLUMN loyalty_discount_minor INTEGER NOT NULL DEFAULT 0;

-- Pillar 6: digital orders (intake only; a person converts them to a sale).
CREATE TABLE digital_orders (
  order_id             TEXT PRIMARY KEY,
  order_number         TEXT NOT NULL UNIQUE,
  branch_id            TEXT NOT NULL,
  channel              TEXT NOT NULL CHECK (channel IN ('phone','whatsapp','web','other')),
  external_ref         TEXT,
  customer_id          TEXT REFERENCES customers(customer_id),
  phone                TEXT,
  status               TEXT NOT NULL CHECK (status IN ('draft','confirmed','converted','cancelled')),
  payment_state        TEXT NOT NULL CHECK (payment_state IN ('unpaid','recorded','screenshot_pending')),
  inbox_seq            INTEGER,
  note                 TEXT,
  address              TEXT,
  delivery_wanted      INTEGER NOT NULL DEFAULT 0,
  sale_id              TEXT,
  delivery_id          TEXT,
  convert_operation_id TEXT UNIQUE,
  created_by           TEXT NOT NULL,
  created_at           TEXT NOT NULL,
  updated_at           TEXT NOT NULL
);
CREATE INDEX ix_digital_orders_status ON digital_orders(status, created_at);
CREATE TABLE digital_order_lines (
  order_id    TEXT NOT NULL REFERENCES digital_orders(order_id),
  line_no     INTEGER NOT NULL,
  product_id  TEXT REFERENCES products(product_id),
  description TEXT NOT NULL,
  qty_milli   INTEGER NOT NULL CHECK (qty_milli > 0),
  PRIMARY KEY (order_id, line_no)
);

-- Pillar 10: users may work in more than one branch.
CREATE TABLE user_branches (
  user_id   TEXT NOT NULL REFERENCES users(user_id),
  branch_id TEXT NOT NULL REFERENCES branches(branch_id),
  PRIMARY KEY (user_id, branch_id)
);

-- Pillar 8: saved date-range presets (this computer only).
CREATE TABLE report_presets (
  preset_id  TEXT PRIMARY KEY,
  user_id    TEXT NOT NULL,
  name       TEXT NOT NULL,
  range_kind TEXT NOT NULL,
  from_date  TEXT,
  to_date    TEXT,
  created_at TEXT NOT NULL
);

-- Pillar 11: short-lived companion (phone) tokens, hub only, stored hashed.
CREATE TABLE companion_tokens (
  token_hash TEXT PRIMARY KEY,
  user_id    TEXT NOT NULL,
  label      TEXT,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  revoked_at TEXT,
  last_used_at TEXT
);
