-- Wave 4 of the merchant operating system: procurement (docs/PROCUREMENT.md).
-- Demand → days of stock → suggested order → requisition → approval →
-- purchase order → receiving with differences → supplier document →
-- three-way match → payable → supplier return and credit.
--
-- Nothing is invented for the past. The upgrade creates no minimum order,
-- lead time, preferred supplier, requisition, approval, difference or
-- return. Existing purchase orders and receipts stay valid as they are, and
-- a draft purchase order is not marked approved.

-- ---------------------------------------------------------------- supplier catalogue
-- One row per supplier and product: the terms on which the product is
-- ordered from that supplier. `supplier_product_map` stays what it is: the
-- evidence of how the supplier's documents name the product (item codes and
-- descriptions, many per product). Both are read through one model
-- (`catalogue.rs`).
CREATE TABLE supplier_products (
  supplier_id      TEXT NOT NULL REFERENCES suppliers(supplier_id),
  product_id       TEXT NOT NULL REFERENCES products(product_id),
  supplier_code    TEXT,
  -- Single units in one pack (case) as the supplier sells it.
  units_per_case   INTEGER CHECK (units_per_case IS NULL OR units_per_case > 0),
  -- 'person': typed or confirmed by a person (wins); 'document': confirmed
  -- while reviewing a supplier document.
  pack_source      TEXT CHECK (pack_source IN ('person','document')),
  -- Minimum order, in packs (cases) when a pack size is known, else in units.
  moq_packs        INTEGER CHECK (moq_packs IS NULL OR moq_packs > 0),
  lead_time_days   INTEGER CHECK (lead_time_days IS NULL OR (lead_time_days >= 0 AND lead_time_days <= 365)),
  preferred        INTEGER NOT NULL DEFAULT 0 CHECK (preferred IN (0,1)),
  active           INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0,1)),
  terms_confirmed_by TEXT,
  terms_confirmed_at TEXT,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL,
  version          INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (supplier_id, product_id)
);
CREATE INDEX ix_supplier_products_product ON supplier_products(product_id, active);
-- At most one preferred supplier per product.
CREATE UNIQUE INDEX ux_supplier_products_preferred ON supplier_products(product_id) WHERE preferred = 1;

-- Seed from facts only: who has supplied what (document mappings, purchase
-- orders, receipts). A pack size is carried over only where the documents
-- confirmed exactly one; terms (minimum, lead time, preferred) stay empty.
INSERT OR IGNORE INTO supplier_products(supplier_id, product_id, units_per_case, pack_source, created_at, updated_at)
  SELECT m.supplier_id, m.product_id,
         CASE WHEN COUNT(DISTINCT m.units_per_case) = 1 THEN MAX(m.units_per_case) END,
         CASE WHEN COUNT(DISTINCT m.units_per_case) = 1 THEN 'document' END,
         strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now')
  FROM supplier_product_map m
  JOIN suppliers s ON s.supplier_id = m.supplier_id
  JOIN products p ON p.product_id = m.product_id
  WHERE m.units_per_case IS NULL OR m.units_per_case > 0
  GROUP BY m.supplier_id, m.product_id;
INSERT OR IGNORE INTO supplier_products(supplier_id, product_id, created_at, updated_at)
  SELECT DISTINCT o.supplier_id, i.product_id, strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now')
  FROM purchase_order_items i JOIN purchase_orders o ON o.po_id = i.po_id
  JOIN products p ON p.product_id = i.product_id;
INSERT OR IGNORE INTO supplier_products(supplier_id, product_id, created_at, updated_at)
  SELECT DISTINCT g.supplier_id, i.product_id, strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now')
  FROM goods_receipt_items i JOIN goods_receipts g ON g.receipt_id = i.receipt_id
  JOIN suppliers s ON s.supplier_id = g.supplier_id
  JOIN products p ON p.product_id = i.product_id
  WHERE g.supplier_id IS NOT NULL;

-- A product's maximum stock (order up to), when the owner sets one.
ALTER TABLE products ADD COLUMN max_stock_milli INTEGER CHECK (max_stock_milli IS NULL OR max_stock_milli > 0);

-- ---------------------------------------------------------------- requisitions
CREATE TABLE requisitions (
  requisition_id   TEXT PRIMARY KEY,
  number           TEXT NOT NULL UNIQUE,
  branch_id        TEXT NOT NULL,
  status           TEXT NOT NULL CHECK (status IN ('draft','submitted','approved','rejected','cancelled','converted')),
  note             TEXT,
  created_by       TEXT NOT NULL,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL,
  submitted_by     TEXT,
  submitted_at     TEXT,
  decided_by       TEXT,
  decided_at       TEXT,
  decision_note    TEXT,
  converted_by     TEXT,
  converted_at     TEXT,
  convert_operation_id TEXT UNIQUE,
  create_operation_id  TEXT UNIQUE,
  version          INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX ix_requisitions_status ON requisitions(branch_id, status);

CREATE TABLE requisition_lines (
  line_id          TEXT PRIMARY KEY,
  requisition_id   TEXT NOT NULL REFERENCES requisitions(requisition_id),
  line_no          INTEGER NOT NULL,
  product_id       TEXT NOT NULL REFERENCES products(product_id),
  supplier_id      TEXT REFERENCES suppliers(supplier_id),
  qty_milli        INTEGER NOT NULL CHECK (qty_milli > 0),
  packs            INTEGER,
  units_per_case   INTEGER CHECK (units_per_case IS NULL OR units_per_case > 0),
  unit_cost_minor  INTEGER CHECK (unit_cost_minor IS NULL OR unit_cost_minor >= 0),
  source           TEXT NOT NULL CHECK (source IN ('manual','replenishment')),
  -- For a suggested line: the figures and reasons the engine used.
  evidence_json    TEXT,
  po_id            TEXT,
  po_item_id       TEXT,
  UNIQUE (requisition_id, product_id)
);
CREATE INDEX ix_requisition_lines_req ON requisition_lines(requisition_id, line_no);
CREATE INDEX ix_requisition_lines_product ON requisition_lines(product_id);

-- ---------------------------------------------------------------- purchase orders
ALTER TABLE purchase_orders ADD COLUMN requisition_id TEXT;
ALTER TABLE purchase_order_items ADD COLUMN requisition_line_id TEXT;
-- Quantity a person decided will not come (shortage cancelled, order closed short).
ALTER TABLE purchase_order_items ADD COLUMN qty_cancelled_milli INTEGER NOT NULL DEFAULT 0 CHECK (qty_cancelled_milli >= 0);
CREATE INDEX ix_po_items_product ON purchase_order_items(product_id);
CREATE INDEX ix_po_requisition ON purchase_orders(requisition_id);

-- Durable approvals. An approval covers the exact order it saw (the
-- fingerprint of supplier, lines, quantities, costs, taxes and totals); a
-- material edit invalidates it, and the history is kept.
CREATE TABLE purchase_order_approvals (
  approval_id      TEXT PRIMARY KEY,
  po_id            TEXT NOT NULL REFERENCES purchase_orders(po_id),
  po_version       INTEGER NOT NULL,
  fingerprint      TEXT NOT NULL,
  total_minor      INTEGER NOT NULL,
  policy_mode      TEXT NOT NULL,
  threshold_minor  INTEGER,
  approved_by      TEXT NOT NULL,
  approved_at      TEXT NOT NULL,
  note             TEXT,
  invalidated_at   TEXT,
  invalidated_reason TEXT,
  operation_id     TEXT NOT NULL UNIQUE
);
CREATE INDEX ix_po_approvals_po ON purchase_order_approvals(po_id, approved_at);

-- ---------------------------------------------------------------- receiving
-- What was delivered for a received line (accepted + rejected); NULL on
-- receipts made before Wave 4, when only the accepted quantity was kept.
ALTER TABLE goods_receipt_items ADD COLUMN qty_delivered_milli INTEGER;
-- A substitution: product B received against order line A (product A).
ALTER TABLE goods_receipt_items ADD COLUMN substitute_for_product_id TEXT;
CREATE INDEX ix_receipt_items_receipt ON goods_receipt_items(receipt_id);
CREATE INDEX ix_receipt_items_po_item ON goods_receipt_items(po_item_id);
CREATE INDEX ix_goods_receipts_po ON goods_receipts(po_id);

-- Everything at a delivery that differs from the order. Rejected goods are
-- recorded here and never become stock (and are never recorded as waste).
CREATE TABLE receipt_discrepancies (
  discrepancy_id   TEXT PRIMARY KEY,
  receipt_id       TEXT NOT NULL REFERENCES goods_receipts(receipt_id),
  po_id            TEXT,
  po_item_id       TEXT,
  supplier_id      TEXT,
  -- The ordered product (for a substitution: A).
  product_id       TEXT NOT NULL,
  -- For a substitution: the product received instead (B).
  substitute_product_id TEXT,
  kind             TEXT NOT NULL CHECK (kind IN ('shortage','overage','damaged','rejected','substitution')),
  qty_milli        INTEGER NOT NULL CHECK (qty_milli > 0),
  reason           TEXT CHECK (reason IS NULL OR reason IN ('damaged','wrong_item','short_dated','expired','quality','not_ordered','other')),
  -- shortage: open → backorder | cancelled; overage: accepted (within
  -- tolerance or approved) | rejected; damaged (kept): accepted;
  -- rejected: rejected; substitution: accepted.
  resolution       TEXT NOT NULL CHECK (resolution IN ('open','backorder','cancelled','accepted','rejected')),
  approved_by      TEXT,
  resolved_by      TEXT,
  resolved_at      TEXT,
  note             TEXT,
  created_by       TEXT NOT NULL,
  created_at       TEXT NOT NULL
);
CREATE INDEX ix_discrepancies_receipt ON receipt_discrepancies(receipt_id);
CREATE INDEX ix_discrepancies_po ON receipt_discrepancies(po_id);
CREATE INDEX ix_discrepancies_supplier ON receipt_discrepancies(supplier_id, created_at);
CREATE INDEX ix_discrepancies_open ON receipt_discrepancies(resolution) WHERE resolution = 'open';

-- ---------------------------------------------------------------- three-way match
-- A person's acceptance of a supplier document's match result. It covers the
-- result it saw (fingerprint); if the evidence changes, it no longer applies.
ALTER TABLE supplier_invoices ADD COLUMN match_accepted_by TEXT;
ALTER TABLE supplier_invoices ADD COLUMN match_accepted_at TEXT;
ALTER TABLE supplier_invoices ADD COLUMN match_accepted_fingerprint TEXT;
ALTER TABLE supplier_invoices ADD COLUMN match_accepted_note TEXT;
CREATE INDEX ix_supplier_invoices_po ON supplier_invoices(po_id);

-- ---------------------------------------------------------------- supplier returns
CREATE TABLE supplier_returns (
  return_id        TEXT PRIMARY KEY,
  number           TEXT NOT NULL UNIQUE,
  supplier_id      TEXT NOT NULL REFERENCES suppliers(supplier_id),
  branch_id        TEXT NOT NULL,
  receipt_id       TEXT,
  po_id            TEXT,
  status           TEXT NOT NULL CHECK (status IN ('draft','confirmed','credited','cancelled','reversed')),
  note             TEXT,
  -- What the supplier is expected to credit (at the cost the goods came in).
  expected_credit_minor INTEGER NOT NULL DEFAULT 0 CHECK (expected_credit_minor >= 0),
  -- The supplier's credit note (a supplier_invoices row, doc_type credit_note).
  credit_invoice_id TEXT UNIQUE,
  created_by       TEXT NOT NULL,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL,
  confirmed_by     TEXT,
  confirmed_at     TEXT,
  confirm_operation_id TEXT UNIQUE,
  reversed_by      TEXT,
  reversed_at      TEXT,
  reversal_reason  TEXT,
  reverse_operation_id TEXT UNIQUE,
  version          INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX ix_supplier_returns_supplier ON supplier_returns(supplier_id, status);

CREATE TABLE supplier_return_lines (
  line_id          TEXT PRIMARY KEY,
  return_id        TEXT NOT NULL REFERENCES supplier_returns(return_id),
  line_no          INTEGER NOT NULL,
  product_id       TEXT NOT NULL REFERENCES products(product_id),
  lot_id           TEXT,
  qty_milli        INTEGER NOT NULL CHECK (qty_milli > 0),
  unit_cost_minor  INTEGER NOT NULL CHECK (unit_cost_minor >= 0),
  reason           TEXT NOT NULL CHECK (reason IN ('damaged','incorrect_item','over_delivery','short_dated','expired','quality','recalled','commercial','other')),
  movement_id      TEXT,
  reversal_movement_id TEXT
);
CREATE INDEX ix_supplier_return_lines_ret ON supplier_return_lines(return_id, line_no);
CREATE INDEX ix_supplier_return_lines_product ON supplier_return_lines(product_id);

-- ---------------------------------------------------------------- settings
-- The cost tolerance had one home (inventory → supplier documents); it moves
-- to purchasing so one rule serves documents, receiving and the match.
INSERT OR IGNORE INTO settings(key, value_json, updated_at)
  SELECT 'purchasing', json_object('cost_tolerance_bp', json_extract(value_json, '$.invoice_cost_variance_bp')), strftime('%Y-%m-%dT%H:%M:%SZ','now')
  FROM settings WHERE key = 'inventory' AND json_extract(value_json, '$.invoice_cost_variance_bp') IS NOT NULL;
