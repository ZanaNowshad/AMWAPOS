-- Wave 6 of the merchant operating system: promotions, coupons and virtual
-- bundles (docs/PROMOTIONS_AND_BUNDLES.md).
--
-- Nothing is invented for the past: no promotion, coupon, redemption or
-- bundle is created; old manual discounts stay manual discounts; existing
-- sales, receipts and prices are unchanged. Every table starts empty.

-- ---------------------------------------------------------------- promotions
-- A promotion temporarily changes what a sale pays. It never changes a
-- product's normal (retail or channel) price.
-- Lifecycle: draft -> active <-> paused -> ended; archived hides it. Whether
-- it applies now is derived from status + schedule, never stored.
-- Schedule: store-local date-times 'YYYY-MM-DDTHH:MM' (start inclusive, end
-- exclusive), compared with the sale's time in the store's timezone.
CREATE TABLE promotions (
  promotion_id      TEXT PRIMARY KEY,
  name              TEXT NOT NULL,
  name_ar           TEXT,
  description       TEXT,
  status            TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft','active','paused','ended','archived')),
  kind              TEXT NOT NULL CHECK (kind IN ('percent','amount','fixed_price','quantity','bxgy','basket')),
  -- 'items' (products/categories listed in promotion_targets) or 'all'.
  target            TEXT NOT NULL DEFAULT 'items' CHECK (target IN ('items','all')),
  starts_at         TEXT,
  ends_at           TEXT,
  -- JSON arrays; NULL means every branch / every channel.
  branches_json     TEXT,
  channels_json     TEXT,
  priority          INTEGER NOT NULL DEFAULT 0,
  -- May combine with a promotion of another layer (item / basket / coupon)
  -- on the same line, only when both sides allow it.
  stackable         INTEGER NOT NULL DEFAULT 0 CHECK (stackable IN (0,1)),
  requires_coupon   INTEGER NOT NULL DEFAULT 0 CHECK (requires_coupon IN (0,1)),
  percent_bp        INTEGER CHECK (percent_bp IS NULL OR percent_bp BETWEEN 1 AND 10000),
  amount_minor      INTEGER CHECK (amount_minor IS NULL OR amount_minor > 0),
  price_minor       INTEGER CHECK (price_minor IS NULL OR price_minor >= 0),
  buy_qty           INTEGER CHECK (buy_qty IS NULL OR buy_qty BETWEEN 1 AND 1000),
  get_qty           INTEGER CHECK (get_qty IS NULL OR get_qty BETWEEN 1 AND 1000),
  max_uses          INTEGER CHECK (max_uses IS NULL OR max_uses BETWEEN 1 AND 1000),
  threshold_minor   INTEGER CHECK (threshold_minor IS NULL OR threshold_minor > 0),
  created_by        TEXT NOT NULL,
  created_at        TEXT NOT NULL,
  updated_at        TEXT NOT NULL,
  version           INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX ix_promotions_status ON promotions(status, target);
CREATE TRIGGER trg_promotions_no_delete BEFORE DELETE ON promotions BEGIN SELECT RAISE(ABORT, 'promotions are archived, not deleted'); END;

-- What a promotion looks at: products or categories, as qualifying items
-- ('buy') or, for Buy-X-Get-Y, the reward items ('get').
CREATE TABLE promotion_targets (
  promotion_id TEXT NOT NULL REFERENCES promotions(promotion_id),
  role         TEXT NOT NULL CHECK (role IN ('buy','get')),
  ref_kind     TEXT NOT NULL CHECK (ref_kind IN ('product','category')),
  ref_id       TEXT NOT NULL,
  PRIMARY KEY (promotion_id, role, ref_kind, ref_id)
);
CREATE INDEX ix_promotion_targets_ref ON promotion_targets(ref_id, ref_kind);

-- ---------------------------------------------------------------- coupons
-- A coupon is a key that unlocks a promotion; the promotion computes the
-- money. 'reusable': a public code, redeemable offline on any till.
-- 'limited': at most max_redemptions in total, so only the main computer
-- (which holds every redemption) can accept it.
CREATE TABLE coupons (
  coupon_id       TEXT PRIMARY KEY,
  promotion_id    TEXT NOT NULL REFERENCES promotions(promotion_id),
  code            TEXT NOT NULL,
  code_norm       TEXT NOT NULL UNIQUE,
  kind            TEXT NOT NULL CHECK (kind IN ('reusable','limited')),
  max_redemptions INTEGER CHECK (max_redemptions IS NULL OR max_redemptions >= 1),
  active          INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
  created_by      TEXT NOT NULL,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  version         INTEGER NOT NULL DEFAULT 1,
  CHECK ((kind = 'limited') = (max_redemptions IS NOT NULL))
);
CREATE INDEX ix_coupons_promotion ON coupons(promotion_id);
CREATE TRIGGER trg_coupons_no_delete BEFORE DELETE ON coupons BEGIN SELECT RAISE(ABORT, 'coupons are switched off, not deleted'); END;

-- One row per committed sale that used a coupon (never for a preview, a
-- held or an abandoned cart). Append-only.
CREATE TABLE coupon_redemptions (
  redemption_id TEXT PRIMARY KEY,
  coupon_id     TEXT NOT NULL,
  promotion_id  TEXT NOT NULL,
  code          TEXT NOT NULL,
  sale_id       TEXT NOT NULL,
  branch_id     TEXT NOT NULL,
  device_id     TEXT NOT NULL,
  user_id       TEXT NOT NULL,
  customer_id   TEXT,
  amount_minor  INTEGER NOT NULL CHECK (amount_minor >= 0),
  operation_id  TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  UNIQUE (coupon_id, sale_id)
);
CREATE INDEX ix_coupon_redemptions_coupon ON coupon_redemptions(coupon_id);
CREATE TRIGGER trg_coupon_redemptions_no_update BEFORE UPDATE ON coupon_redemptions BEGIN SELECT RAISE(ABORT, 'redemptions are permanent'); END;
CREATE TRIGGER trg_coupon_redemptions_no_delete BEFORE DELETE ON coupon_redemptions BEGIN SELECT RAISE(ABORT, 'redemptions are permanent'); END;

-- The coupon typed on a sale in progress (a preview: it redeems nothing).
ALTER TABLE carts ADD COLUMN coupon_code TEXT;

-- ---------------------------------------------------------------- applied promotions
-- What each promotion took off each sale line, frozen with the sale.
CREATE TABLE sale_item_promotions (
  sale_item_id   TEXT NOT NULL,
  promotion_id   TEXT NOT NULL,
  sale_id        TEXT NOT NULL,
  layer          TEXT NOT NULL CHECK (layer IN ('item','basket','coupon')),
  promotion_name TEXT NOT NULL,
  coupon_code    TEXT,
  amount_minor   INTEGER NOT NULL CHECK (amount_minor >= 0),
  PRIMARY KEY (sale_item_id, promotion_id)
);
CREATE INDEX ix_sale_item_promotions_promo ON sale_item_promotions(promotion_id);
CREATE INDEX ix_sale_item_promotions_sale ON sale_item_promotions(sale_id);
CREATE TRIGGER trg_sale_item_promotions_no_update BEFORE UPDATE ON sale_item_promotions BEGIN SELECT RAISE(ABORT, 'sale history is immutable'); END;
CREATE TRIGGER trg_sale_item_promotions_no_delete BEFORE DELETE ON sale_item_promotions BEGIN SELECT RAISE(ABORT, 'sale history is immutable'); END;
-- The promotion part of each sale line's discount (NULL on older sales).
ALTER TABLE sale_items ADD COLUMN promo_discount_minor INTEGER;

-- ---------------------------------------------------------------- virtual bundles
-- A bundle is a sellable product made of fixed component quantities taken
-- from stock when it is sold. It has no stock of its own. Each change makes
-- a new version; sales keep the version they sold.
CREATE TABLE bundles (
  bundle_product_id TEXT PRIMARY KEY REFERENCES products(product_id),
  version           INTEGER NOT NULL DEFAULT 1,
  active            INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
  created_by        TEXT NOT NULL,
  created_at        TEXT NOT NULL,
  updated_by        TEXT NOT NULL,
  updated_at        TEXT NOT NULL
);
CREATE TRIGGER trg_bundles_no_delete BEFORE DELETE ON bundles BEGIN SELECT RAISE(ABORT, 'bundles are switched off, not deleted'); END;
CREATE TABLE bundle_components (
  bundle_product_id    TEXT NOT NULL REFERENCES bundles(bundle_product_id),
  version              INTEGER NOT NULL,
  component_product_id TEXT NOT NULL REFERENCES products(product_id),
  qty_milli            INTEGER NOT NULL CHECK (qty_milli > 0),
  PRIMARY KEY (bundle_product_id, version, component_product_id)
);
CREATE INDEX ix_bundle_components_component ON bundle_components(component_product_id);
CREATE TRIGGER trg_bundle_components_no_update BEFORE UPDATE ON bundle_components BEGIN SELECT RAISE(ABORT, 'a bundle version is fixed; save a new version'); END;
CREATE TRIGGER trg_bundle_components_no_delete BEFORE DELETE ON bundle_components BEGIN SELECT RAISE(ABORT, 'a bundle version is fixed; save a new version'); END;

-- The bundle version a sale in progress was priced with.
ALTER TABLE cart_lines ADD COLUMN bundle_version INTEGER;
-- A bundle sold is kept as its component lines (stock, cost, VAT, refunds),
-- grouped by the bundle line they came from.
ALTER TABLE sale_items ADD COLUMN bundle_product_id TEXT;
ALTER TABLE sale_items ADD COLUMN bundle_name TEXT;
ALTER TABLE sale_items ADD COLUMN bundle_version INTEGER;
ALTER TABLE sale_items ADD COLUMN bundle_line_no INTEGER;
ALTER TABLE sale_items ADD COLUMN bundle_qty_milli INTEGER;
ALTER TABLE sale_items ADD COLUMN bundle_component_qty_milli INTEGER;
CREATE INDEX ix_sale_items_bundle ON sale_items(bundle_product_id) WHERE bundle_product_id IS NOT NULL;

-- ---------------------------------------------------------------- sync
CREATE TRIGGER trg_sync_promotions_insert AFTER INSERT ON promotions WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('promotions', json_object('promotion_id', NEW.promotion_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_promotions_update AFTER UPDATE ON promotions WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('promotions', json_object('promotion_id', NEW.promotion_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_promotion_targets_insert AFTER INSERT ON promotion_targets WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('promotion_targets', json_object('promotion_id', NEW.promotion_id, 'role', NEW.role, 'ref_kind', NEW.ref_kind, 'ref_id', NEW.ref_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_promotion_targets_update AFTER UPDATE ON promotion_targets WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('promotion_targets', json_object('promotion_id', NEW.promotion_id, 'role', NEW.role, 'ref_kind', NEW.ref_kind, 'ref_id', NEW.ref_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_promotion_targets_delete AFTER DELETE ON promotion_targets WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('promotion_targets', json_object('promotion_id', OLD.promotion_id, 'role', OLD.role, 'ref_kind', OLD.ref_kind, 'ref_id', OLD.ref_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_coupons_insert AFTER INSERT ON coupons WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('coupons', json_object('coupon_id', NEW.coupon_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_coupons_update AFTER UPDATE ON coupons WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('coupons', json_object('coupon_id', NEW.coupon_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_coupon_redemptions_insert AFTER INSERT ON coupon_redemptions WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('coupon_redemptions', json_object('redemption_id', NEW.redemption_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_sale_item_promotions_insert AFTER INSERT ON sale_item_promotions WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('sale_item_promotions', json_object('sale_item_id', NEW.sale_item_id, 'promotion_id', NEW.promotion_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_bundles_insert AFTER INSERT ON bundles WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('bundles', json_object('bundle_product_id', NEW.bundle_product_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_bundles_update AFTER UPDATE ON bundles WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('bundles', json_object('bundle_product_id', NEW.bundle_product_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_bundle_components_insert AFTER INSERT ON bundle_components WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('bundle_components', json_object('bundle_product_id', NEW.bundle_product_id, 'version', NEW.version, 'component_product_id', NEW.component_product_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
