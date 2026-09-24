-- AMWAPOS schema v2: multi-terminal replication support.
--
-- Change capture: AFTER triggers append (table, primary key) to sync_outbox.
-- The row payload is read when a change is shipped, so the outbox never holds
-- stale copies of mutable rows. Triggers are suppressed while a terminal
-- applies changes pulled from its hub (sync_control 'suppress' = '1') and
-- attribute rows applied on the hub to their originating device
-- (sync_control 'origin').
-- This file is immutable once released.

CREATE TABLE sync_control (
  k TEXT PRIMARY KEY,
  v TEXT
);
INSERT INTO sync_control(k, v) VALUES ('suppress', '0'), ('origin', NULL);

CREATE TABLE sync_outbox (
  seq        INTEGER PRIMARY KEY AUTOINCREMENT,
  table_name TEXT NOT NULL,
  row_pk     TEXT NOT NULL,
  op         TEXT NOT NULL CHECK (op IN ('upsert','delete')),
  origin     TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX ix_outbox_row ON sync_outbox(table_name, row_pk);

-- Changes that could not be applied (kept for review and retry, never dropped).
CREATE TABLE sync_dead_letters (
  dead_id     TEXT PRIMARY KEY,
  direction   TEXT NOT NULL CHECK (direction IN ('push','pull','apply')),
  origin      TEXT,
  table_name  TEXT NOT NULL,
  row_pk      TEXT NOT NULL,
  op          TEXT NOT NULL,
  payload_json TEXT,
  error       TEXT NOT NULL,
  attempts    INTEGER NOT NULL DEFAULT 1,
  status      TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved')),
  created_at  TEXT NOT NULL,
  last_attempt_at TEXT NOT NULL
);

-- Hub-side liveness of terminals (local, not replicated).
CREATE TABLE device_heartbeats (
  device_id     TEXT PRIMARY KEY,
  last_seen_at  TEXT NOT NULL,
  app_version   TEXT,
  schema_version INTEGER,
  pending_count INTEGER,
  last_push_at  TEXT,
  last_pull_at  TEXT,
  last_error    TEXT,
  current_user_id TEXT
);

-- One-time pairing codes issued by the hub (hash only).
CREATE TABLE pairing_codes (
  code_hash   TEXT PRIMARY KEY,
  branch_id   TEXT NOT NULL,
  device_name TEXT,
  created_by  TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  expires_at  TEXT NOT NULL,
  used_at     TEXT,
  used_by_device TEXT
);

-- business: hub
CREATE TRIGGER trg_sync_business_insert AFTER INSERT ON business WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('business', json_object('business_id', NEW.business_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_business_update AFTER UPDATE ON business WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('business', json_object('business_id', NEW.business_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_business_delete AFTER DELETE ON business WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('business', json_object('business_id', OLD.business_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- branches: hub
CREATE TRIGGER trg_sync_branches_insert AFTER INSERT ON branches WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('branches', json_object('branch_id', NEW.branch_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_branches_update AFTER UPDATE ON branches WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('branches', json_object('branch_id', NEW.branch_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_branches_delete AFTER DELETE ON branches WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('branches', json_object('branch_id', OLD.branch_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- devices: hub
CREATE TRIGGER trg_sync_devices_insert AFTER INSERT ON devices WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('devices', json_object('device_id', NEW.device_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_devices_update AFTER UPDATE ON devices WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('devices', json_object('device_id', NEW.device_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_devices_delete AFTER DELETE ON devices WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('devices', json_object('device_id', OLD.device_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- settings: hub
CREATE TRIGGER trg_sync_settings_insert AFTER INSERT ON settings WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1' AND NEW.key NOT LIKE 'local.%'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('settings', json_object('key', NEW.key), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_settings_update AFTER UPDATE ON settings WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1' AND NEW.key NOT LIKE 'local.%'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('settings', json_object('key', NEW.key), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_settings_delete AFTER DELETE ON settings WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1' AND OLD.key NOT LIKE 'local.%'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('settings', json_object('key', OLD.key), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- roles: hub
CREATE TRIGGER trg_sync_roles_insert AFTER INSERT ON roles WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('roles', json_object('role_id', NEW.role_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_roles_update AFTER UPDATE ON roles WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('roles', json_object('role_id', NEW.role_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_roles_delete AFTER DELETE ON roles WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('roles', json_object('role_id', OLD.role_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- permissions: hub
CREATE TRIGGER trg_sync_permissions_insert AFTER INSERT ON permissions WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('permissions', json_object('code', NEW.code), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_permissions_update AFTER UPDATE ON permissions WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('permissions', json_object('code', NEW.code), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_permissions_delete AFTER DELETE ON permissions WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('permissions', json_object('code', OLD.code), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- role_permissions: hub
CREATE TRIGGER trg_sync_role_permissions_insert AFTER INSERT ON role_permissions WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('role_permissions', json_object('role_id', NEW.role_id, 'permission_code', NEW.permission_code), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_role_permissions_update AFTER UPDATE ON role_permissions WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('role_permissions', json_object('role_id', NEW.role_id, 'permission_code', NEW.permission_code), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_role_permissions_delete AFTER DELETE ON role_permissions WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('role_permissions', json_object('role_id', OLD.role_id, 'permission_code', OLD.permission_code), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- users: hub
CREATE TRIGGER trg_sync_users_insert AFTER INSERT ON users WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('users', json_object('user_id', NEW.user_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_users_update AFTER UPDATE ON users WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('users', json_object('user_id', NEW.user_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_users_delete AFTER DELETE ON users WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('users', json_object('user_id', OLD.user_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- categories: hub
CREATE TRIGGER trg_sync_categories_insert AFTER INSERT ON categories WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('categories', json_object('category_id', NEW.category_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_categories_update AFTER UPDATE ON categories WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('categories', json_object('category_id', NEW.category_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_categories_delete AFTER DELETE ON categories WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('categories', json_object('category_id', OLD.category_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- tax_rules: hub
CREATE TRIGGER trg_sync_tax_rules_insert AFTER INSERT ON tax_rules WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('tax_rules', json_object('tax_rule_id', NEW.tax_rule_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_tax_rules_update AFTER UPDATE ON tax_rules WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('tax_rules', json_object('tax_rule_id', NEW.tax_rule_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_tax_rules_delete AFTER DELETE ON tax_rules WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('tax_rules', json_object('tax_rule_id', OLD.tax_rule_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- products: hub
CREATE TRIGGER trg_sync_products_insert AFTER INSERT ON products WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('products', json_object('product_id', NEW.product_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_products_update AFTER UPDATE ON products WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('products', json_object('product_id', NEW.product_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_products_delete AFTER DELETE ON products WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('products', json_object('product_id', OLD.product_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- product_barcodes: hub
CREATE TRIGGER trg_sync_product_barcodes_insert AFTER INSERT ON product_barcodes WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_barcodes', json_object('barcode_id', NEW.barcode_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_product_barcodes_update AFTER UPDATE ON product_barcodes WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_barcodes', json_object('barcode_id', NEW.barcode_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_product_barcodes_delete AFTER DELETE ON product_barcodes WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_barcodes', json_object('barcode_id', OLD.barcode_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- product_prices: hub
CREATE TRIGGER trg_sync_product_prices_insert AFTER INSERT ON product_prices WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_prices', json_object('price_id', NEW.price_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_product_prices_update AFTER UPDATE ON product_prices WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_prices', json_object('price_id', NEW.price_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_product_prices_delete AFTER DELETE ON product_prices WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_prices', json_object('price_id', OLD.price_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- product_cost_history: hub
CREATE TRIGGER trg_sync_product_cost_history_insert AFTER INSERT ON product_cost_history WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_cost_history', json_object('cost_id', NEW.cost_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_product_cost_history_update AFTER UPDATE ON product_cost_history WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_cost_history', json_object('cost_id', NEW.cost_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_product_cost_history_delete AFTER DELETE ON product_cost_history WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_cost_history', json_object('cost_id', OLD.cost_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- product_costs: hub
CREATE TRIGGER trg_sync_product_costs_insert AFTER INSERT ON product_costs WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_costs', json_object('product_id', NEW.product_id, 'branch_id', NEW.branch_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_product_costs_update AFTER UPDATE ON product_costs WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_costs', json_object('product_id', NEW.product_id, 'branch_id', NEW.branch_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_product_costs_delete AFTER DELETE ON product_costs WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('product_costs', json_object('product_id', OLD.product_id, 'branch_id', OLD.branch_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- suppliers: hub
CREATE TRIGGER trg_sync_suppliers_insert AFTER INSERT ON suppliers WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('suppliers', json_object('supplier_id', NEW.supplier_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_suppliers_update AFTER UPDATE ON suppliers WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('suppliers', json_object('supplier_id', NEW.supplier_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_suppliers_delete AFTER DELETE ON suppliers WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('suppliers', json_object('supplier_id', OLD.supplier_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- sales: append
CREATE TRIGGER trg_sync_sales_insert AFTER INSERT ON sales WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('sales', json_object('sale_id', NEW.sale_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- sale_items: append
CREATE TRIGGER trg_sync_sale_items_insert AFTER INSERT ON sale_items WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('sale_items', json_object('sale_item_id', NEW.sale_item_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- payments: append
CREATE TRIGGER trg_sync_payments_insert AFTER INSERT ON payments WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('payments', json_object('payment_id', NEW.payment_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- refunds: append
CREATE TRIGGER trg_sync_refunds_insert AFTER INSERT ON refunds WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('refunds', json_object('refund_id', NEW.refund_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- refund_items: append
CREATE TRIGGER trg_sync_refund_items_insert AFTER INSERT ON refund_items WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('refund_items', json_object('refund_item_id', NEW.refund_item_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- refund_tenders: append
CREATE TRIGGER trg_sync_refund_tenders_insert AFTER INSERT ON refund_tenders WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('refund_tenders', json_object('refund_tender_id', NEW.refund_tender_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- cash_events: append
CREATE TRIGGER trg_sync_cash_events_insert AFTER INSERT ON cash_events WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('cash_events', json_object('cash_event_id', NEW.cash_event_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- stock_movements: append
CREATE TRIGGER trg_sync_stock_movements_insert AFTER INSERT ON stock_movements WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('stock_movements', json_object('movement_id', NEW.movement_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- customers: shared
CREATE TRIGGER trg_sync_customers_insert AFTER INSERT ON customers WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customers', json_object('customer_id', NEW.customer_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_customers_update AFTER UPDATE ON customers WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customers', json_object('customer_id', NEW.customer_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_customers_delete AFTER DELETE ON customers WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customers', json_object('customer_id', OLD.customer_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- customer_notes: shared
CREATE TRIGGER trg_sync_customer_notes_insert AFTER INSERT ON customer_notes WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customer_notes', json_object('note_id', NEW.note_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_customer_notes_update AFTER UPDATE ON customer_notes WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customer_notes', json_object('note_id', NEW.note_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_customer_notes_delete AFTER DELETE ON customer_notes WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('customer_notes', json_object('note_id', OLD.note_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- shifts: shared
CREATE TRIGGER trg_sync_shifts_insert AFTER INSERT ON shifts WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('shifts', json_object('shift_id', NEW.shift_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_shifts_update AFTER UPDATE ON shifts WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('shifts', json_object('shift_id', NEW.shift_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_shifts_delete AFTER DELETE ON shifts WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('shifts', json_object('shift_id', OLD.shift_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- unknown_barcodes: shared
CREATE TRIGGER trg_sync_unknown_barcodes_insert AFTER INSERT ON unknown_barcodes WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('unknown_barcodes', json_object('barcode', NEW.barcode), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_unknown_barcodes_update AFTER UPDATE ON unknown_barcodes WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('unknown_barcodes', json_object('barcode', NEW.barcode), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_unknown_barcodes_delete AFTER DELETE ON unknown_barcodes WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('unknown_barcodes', json_object('barcode', OLD.barcode), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- delivery_orders: shared
CREATE TRIGGER trg_sync_delivery_orders_insert AFTER INSERT ON delivery_orders WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('delivery_orders', json_object('delivery_id', NEW.delivery_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_delivery_orders_update AFTER UPDATE ON delivery_orders WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('delivery_orders', json_object('delivery_id', NEW.delivery_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_delivery_orders_delete AFTER DELETE ON delivery_orders WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('delivery_orders', json_object('delivery_id', OLD.delivery_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;

-- delivery_events: shared
CREATE TRIGGER trg_sync_delivery_events_insert AFTER INSERT ON delivery_events WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('delivery_events', json_object('event_id', NEW.event_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_delivery_events_update AFTER UPDATE ON delivery_events WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('delivery_events', json_object('event_id', NEW.event_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
CREATE TRIGGER trg_sync_delivery_events_delete AFTER DELETE ON delivery_events WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('delivery_events', json_object('event_id', OLD.event_id), 'delete', (SELECT v FROM sync_control WHERE k='origin'));
END;
