-- Wave 5 of the merchant operating system: the retail commercial foundation
-- (docs/PRICING_AND_CATALOGUE.md). Barcode kinds and PLUs, scale barcodes,
-- duplicate review and product merge, structured address additions, the
-- sales channel, channel prices and pricing policies.
--
-- Nothing is invented for the past: no barcode kind is guessed (NULL = not
-- recorded), no PLU, scale rule, duplicate decision, merge, governorate,
-- channel, channel price or policy is created. Existing sales keep a NULL
-- channel ("not recorded"); existing prices are unchanged.

-- ---------------------------------------------------------------- barcodes and PLU
-- The kind of a barcode, when known. NULL: not recorded (barcodes from before
-- Wave 5 keep NULL; nothing is classified from the digits alone afterwards).
ALTER TABLE product_barcodes ADD COLUMN kind TEXT
  CHECK (kind IS NULL OR kind IN ('ean13','ean8','upc_a','upc_e','code128','internal','supplier'));

-- A PLU (price look-up code) belongs to the product itself, not to its
-- barcodes: digits only, stored without leading zeros, unique among products.
ALTER TABLE products ADD COLUMN plu TEXT CHECK (plu IS NULL OR (plu GLOB '[1-9]*' AND plu NOT GLOB '*[^0-9]*' AND length(plu) <= 12));
CREATE UNIQUE INDEX ux_products_plu ON products(plu) WHERE plu IS NOT NULL;

-- A merged product stays, retired, pointing at the product it became.
ALTER TABLE products ADD COLUMN merged_into_product_id TEXT;
ALTER TABLE products ADD COLUMN merged_at TEXT;

-- ---------------------------------------------------------------- scale barcodes
-- Rules for barcodes printed by scales (weight or price inside the code).
-- Hub-owned, copied to every till so scale labels scan offline. Positions are
-- 1-based, as printed on scale manuals.
CREATE TABLE scale_barcode_rules (
  rule_id        TEXT PRIMARY KEY,
  name           TEXT NOT NULL,
  prefix         TEXT NOT NULL CHECK (length(prefix) BETWEEN 1 AND 6 AND prefix NOT GLOB '*[^0-9]*'),
  length         INTEGER NOT NULL CHECK (length BETWEEN 8 AND 20),
  item_start     INTEGER NOT NULL CHECK (item_start >= 1),
  item_length    INTEGER NOT NULL CHECK (item_length BETWEEN 1 AND 12),
  value_kind     TEXT NOT NULL CHECK (value_kind IN ('weight','price')),
  value_start    INTEGER NOT NULL CHECK (value_start >= 1),
  value_length   INTEGER NOT NULL CHECK (value_length BETWEEN 1 AND 9),
  decimals       INTEGER NOT NULL CHECK (decimals BETWEEN 0 AND 3),
  check_digit    TEXT NOT NULL CHECK (check_digit IN ('none','ean')),
  active         INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
  priority       INTEGER NOT NULL DEFAULT 0,
  created_by     TEXT,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL,
  version        INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX ix_scale_rules_active ON scale_barcode_rules(active, length, prefix);
CREATE TRIGGER trg_sync_scale_barcode_rules_insert AFTER INSERT ON scale_barcode_rules WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('scale_barcode_rules', json_object('rule_id', NEW.rule_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_scale_barcode_rules_update AFTER UPDATE ON scale_barcode_rules WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('scale_barcode_rules', json_object('rule_id', NEW.rule_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_scale_rules_no_delete BEFORE DELETE ON scale_barcode_rules BEGIN SELECT RAISE(ABORT, 'scale rules are switched off, not deleted'); END;

-- What a scale label said, kept on the line it priced.
ALTER TABLE cart_lines ADD COLUMN scale_rule_id TEXT;
ALTER TABLE cart_lines ADD COLUMN scale_value_kind TEXT;
ALTER TABLE cart_lines ADD COLUMN scale_value INTEGER;
ALTER TABLE sale_items ADD COLUMN scale_rule_id TEXT;
ALTER TABLE sale_items ADD COLUMN scale_value_kind TEXT;
ALTER TABLE sale_items ADD COLUMN scale_value INTEGER;
-- Which price list priced the line (retail, or a channel's price). NULL on
-- lines from before Wave 5: not recorded.
ALTER TABLE cart_lines ADD COLUMN price_type TEXT;
ALTER TABLE sale_items ADD COLUMN price_type TEXT;

-- ---------------------------------------------------------------- sales channel
-- Where a sale came from. NULL: not recorded (every sale before Wave 5).
-- Fulfilment (paid here, sent for delivery) stays a separate fact.
ALTER TABLE carts ADD COLUMN channel TEXT CHECK (channel IS NULL OR channel IN ('pos','whatsapp','phone','web','other'));
ALTER TABLE sales ADD COLUMN channel TEXT CHECK (channel IS NULL OR channel IN ('pos','whatsapp','phone','web','other'));
CREATE INDEX ix_sales_channel ON sales(channel, business_date);

-- ---------------------------------------------------------------- prices
-- An applied price can name the policy and batch it came from.
ALTER TABLE product_prices ADD COLUMN policy_id TEXT;
ALTER TABLE product_prices ADD COLUMN batch_id TEXT;
CREATE INDEX ix_prices_resolve ON product_prices(product_id, price_type, branch_id, effective_from);

-- ---------------------------------------------------------------- addresses
-- Governorate (Capital, Muharraq, Northern, Southern) and directions, both
-- optional, next to the existing Flat / Building / Road / Block / landmark.
ALTER TABLE customers ADD COLUMN governorate TEXT CHECK (governorate IS NULL OR governorate IN ('capital','muharraq','northern','southern'));
ALTER TABLE customers ADD COLUMN directions TEXT;
ALTER TABLE delivery_orders ADD COLUMN governorate TEXT CHECK (governorate IS NULL OR governorate IN ('capital','muharraq','northern','southern'));
ALTER TABLE delivery_orders ADD COLUMN directions TEXT;

-- ---------------------------------------------------------------- duplicates and merges
-- A person's decision about a suggested pair (product_a < product_b).
CREATE TABLE product_duplicate_decisions (
  product_a   TEXT NOT NULL,
  product_b   TEXT NOT NULL,
  decision    TEXT NOT NULL CHECK (decision IN ('not_duplicates','later')),
  note        TEXT,
  decided_by  TEXT NOT NULL,
  decided_at  TEXT NOT NULL,
  PRIMARY KEY (product_a, product_b),
  CHECK (product_a < product_b)
);

-- One record per merge: what was moved, which conflicts were decided, by whom.
CREATE TABLE product_merges (
  merge_id          TEXT PRIMARY KEY,
  source_product_id TEXT NOT NULL,
  target_product_id TEXT NOT NULL,
  preview_json      TEXT NOT NULL,
  choices_json      TEXT NOT NULL,
  moved_json        TEXT NOT NULL,
  user_id           TEXT NOT NULL,
  device_id         TEXT,
  operation_id      TEXT NOT NULL UNIQUE,
  created_at        TEXT NOT NULL
);
CREATE INDEX ix_product_merges_source ON product_merges(source_product_id);
CREATE TRIGGER trg_product_merges_no_update BEFORE UPDATE ON product_merges BEGIN SELECT RAISE(ABORT, 'merge records are permanent'); END;
CREATE TRIGGER trg_product_merges_no_delete BEFORE DELETE ON product_merges BEGIN SELECT RAISE(ABORT, 'merge records are permanent'); END;

-- A batch can be carried to the merged product: rebuild stock_lots once to
-- allow provenance 'merge' (lots are kept on the hub only; facts unchanged).
CREATE TABLE stock_lots_new (
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
  provenance        TEXT NOT NULL CHECK (provenance IN ('receiving','count','merge')),
  created_by        TEXT,
  created_at        TEXT NOT NULL,
  merged_from_lot_id TEXT
);
INSERT INTO stock_lots_new(lot_id, lot_number, product_id, branch_id, location_id, supplier_id, po_id, receipt_id, supplier_lot_code, received_at,
    manufactured_on, expires_on, expiry_kind, expiry_source, qty_received_milli, unit_cost_minor, provenance, created_by, created_at)
  SELECT lot_id, lot_number, product_id, branch_id, location_id, supplier_id, po_id, receipt_id, supplier_lot_code, received_at,
    manufactured_on, expires_on, expiry_kind, expiry_source, qty_received_milli, unit_cost_minor, provenance, created_by, created_at
  FROM stock_lots ORDER BY rowid;
CREATE TEMP TABLE _lot_copy_check (ok INTEGER NOT NULL CHECK (ok = 1));
INSERT INTO _lot_copy_check SELECT (SELECT COUNT(*) FROM stock_lots) = (SELECT COUNT(*) FROM stock_lots_new)
  AND (SELECT COALESCE(SUM(qty_received_milli),0) FROM stock_lots) = (SELECT COALESCE(SUM(qty_received_milli),0) FROM stock_lots_new);
DROP TABLE _lot_copy_check;
DROP TABLE stock_lots;
ALTER TABLE stock_lots_new RENAME TO stock_lots;
CREATE INDEX ix_stock_lots_product ON stock_lots(product_id, branch_id);
CREATE INDEX ix_stock_lots_expiry ON stock_lots(expires_on);
CREATE TRIGGER trg_stock_lots_no_update BEFORE UPDATE ON stock_lots BEGIN SELECT RAISE(ABORT, 'lot facts are permanent; record a correction'); END;
CREATE TRIGGER trg_stock_lots_no_delete BEFORE DELETE ON stock_lots BEGIN SELECT RAISE(ABORT, 'lots are kept'); END;

-- ---------------------------------------------------------------- pricing policies
-- Policies recommend prices; they never change a price by themselves.
CREATE TABLE pricing_policies (
  policy_id           TEXT PRIMARY KEY,
  name                TEXT NOT NULL,
  scope               TEXT NOT NULL CHECK (scope IN ('global','category','supplier','branch','channel')),
  scope_id            TEXT,
  markup_bp           INTEGER CHECK (markup_bp IS NULL OR markup_bp BETWEEN 0 AND 100000),
  target_margin_bp    INTEGER CHECK (target_margin_bp IS NULL OR target_margin_bp BETWEEN 0 AND 9500),
  min_margin_bp       INTEGER CHECK (min_margin_bp IS NULL OR min_margin_bp BETWEEN 0 AND 9500),
  rounding_step_minor INTEGER NOT NULL DEFAULT 1 CHECK (rounding_step_minor IN (1,5,10,25,50,100,250,500,1000)),
  ending_minor        INTEGER CHECK (ending_minor IS NULL OR ending_minor BETWEEN 0 AND 999),
  priority            INTEGER NOT NULL DEFAULT 0,
  active              INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
  created_by          TEXT NOT NULL,
  created_at          TEXT NOT NULL,
  updated_at          TEXT NOT NULL,
  version             INTEGER NOT NULL DEFAULT 1,
  CHECK (markup_bp IS NULL OR target_margin_bp IS NULL),
  CHECK ((scope = 'global') = (scope_id IS NULL))
);
CREATE INDEX ix_pricing_policies_scope ON pricing_policies(active, scope, scope_id);

-- A person set a recommendation aside: dismissed while the cost and the
-- suggestion stay the same, or postponed until a date.
CREATE TABLE price_recommendation_decisions (
  product_id      TEXT NOT NULL,
  price_type      TEXT NOT NULL,
  decision        TEXT NOT NULL CHECK (decision IN ('dismissed','postponed')),
  cost_minor      INTEGER,
  suggested_minor INTEGER,
  until           TEXT,
  decided_by      TEXT NOT NULL,
  decided_at      TEXT NOT NULL,
  PRIMARY KEY (product_id, price_type)
);

-- Price changes applied together (bulk), once per operation id.
CREATE TABLE price_change_batches (
  batch_id      TEXT PRIMARY KEY,
  operation_id  TEXT NOT NULL UNIQUE,
  item_count    INTEGER NOT NULL,
  summary_json  TEXT NOT NULL,
  approved_by   TEXT,
  user_id       TEXT NOT NULL,
  created_at    TEXT NOT NULL
);
