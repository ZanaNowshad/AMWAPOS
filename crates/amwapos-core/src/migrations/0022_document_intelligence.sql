-- Document Intelligence: the supplier-invoice scan becomes a reviewable
-- document with provenance, validation and reconciliation. Additive on
-- invoice_scans (the existing pipeline); the scan lines table is rebuilt only
-- to widen the match_kind CHECK and add evidence columns.
--
-- Nothing here posts anything: supplier invoices and receiving drafts are
-- drafts until an authorized person confirms them in the normal workflows.

ALTER TABLE invoice_scans ADD COLUMN stage TEXT NOT NULL DEFAULT 'uploaded'
  CHECK (stage IN ('uploaded','preprocessing','ocr','extracting','matching','validating','ready_for_review','failed'));
UPDATE invoice_scans SET stage = CASE status
  WHEN 'imported' THEN 'uploaded' WHEN 'failed' THEN 'failed' ELSE 'ready_for_review' END;
ALTER TABLE invoice_scans ADD COLUMN source TEXT NOT NULL DEFAULT 'upload' CHECK (source IN ('upload','whatsapp'));
ALTER TABLE invoice_scans ADD COLUMN inbox_seq INTEGER;
ALTER TABLE invoice_scans ADD COLUMN mime TEXT;
ALTER TABLE invoice_scans ADD COLUMN page_count INTEGER;
ALTER TABLE invoice_scans ADD COLUMN doc_type TEXT NOT NULL DEFAULT 'unknown'
  CHECK (doc_type IN ('invoice','credit_note','delivery_note','unknown'));
ALTER TABLE invoice_scans ADD COLUMN doc_type_band TEXT;
ALTER TABLE invoice_scans ADD COLUMN doc_type_source TEXT;
ALTER TABLE invoice_scans ADD COLUMN quality_status TEXT CHECK (quality_status IS NULL OR quality_status IN ('good','usable','poor','unusable'));
ALTER TABLE invoice_scans ADD COLUMN quality_json TEXT;
-- Words with page/line/box, stored as a JSON file next to the original.
ALTER TABLE invoice_scans ADD COLUMN layout_path TEXT;
ALTER TABLE invoice_scans ADD COLUMN fields_json TEXT;
ALTER TABLE invoice_scans ADD COLUMN supplier_match_json TEXT;
ALTER TABLE invoice_scans ADD COLUMN validation_json TEXT;
ALTER TABLE invoice_scans ADD COLUMN duplicate_json TEXT;
ALTER TABLE invoice_scans ADD COLUMN anomalies_json TEXT;
ALTER TABLE invoice_scans ADD COLUMN recon_json TEXT;
ALTER TABLE invoice_scans ADD COLUMN summary TEXT;
ALTER TABLE invoice_scans ADD COLUMN invoice_number_norm TEXT;
ALTER TABLE invoice_scans ADD COLUMN due_date TEXT;
ALTER TABLE invoice_scans ADD COLUMN delivery_date TEXT;
ALTER TABLE invoice_scans ADD COLUMN subtotal_minor INTEGER;
ALTER TABLE invoice_scans ADD COLUMN vat_minor INTEGER;
ALTER TABLE invoice_scans ADD COLUMN supplier_vat TEXT;
ALTER TABLE invoice_scans ADD COLUMN supplier_cr TEXT;
ALTER TABLE invoice_scans ADD COLUMN buyer_vat TEXT;
ALTER TABLE invoice_scans ADD COLUMN line_fingerprint TEXT;
ALTER TABLE invoice_scans ADD COLUMN ai_model TEXT;
ALTER TABLE invoice_scans ADD COLUMN corrections INTEGER NOT NULL DEFAULT 0;
ALTER TABLE invoice_scans ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE invoice_scans ADD COLUMN started_at TEXT;
ALTER TABLE invoice_scans ADD COLUMN finished_at TEXT;
ALTER TABLE invoice_scans ADD COLUMN supplier_invoice_id TEXT;
ALTER TABLE invoice_scans ADD COLUMN receiving_draft_id TEXT;
CREATE INDEX ix_invoice_scans_sha ON invoice_scans(image_sha256);
CREATE INDEX ix_invoice_scans_number ON invoice_scans(supplier_id, invoice_number_norm);
CREATE INDEX ix_invoice_scans_stage ON invoice_scans(stage, created_at);

CREATE TABLE invoice_scan_lines_new (
  scan_id          TEXT NOT NULL REFERENCES invoice_scans(scan_id),
  line_no          INTEGER NOT NULL,
  raw_text         TEXT NOT NULL,
  description      TEXT,
  code             TEXT,
  qty_milli        INTEGER,
  unit_cost_minor  INTEGER,
  line_total_minor INTEGER,
  product_id       TEXT,
  match_kind       TEXT NOT NULL CHECK (match_kind IN ('barcode','supplier_map','supplier_code','sku','name','fuzzy','ai','manual','none')),
  match_score      INTEGER NOT NULL DEFAULT 0,
  include          INTEGER NOT NULL DEFAULT 1,
  barcode          TEXT,
  barcode_valid    INTEGER,
  unit             TEXT,
  case_qty_milli   INTEGER,
  units_per_case   INTEGER,
  base_qty_milli   INTEGER,
  pack_text        TEXT,
  pack_clear       INTEGER NOT NULL DEFAULT 0,
  discount_minor   INTEGER,
  vat_rate_bp      INTEGER,
  vat_minor        INTEGER,
  match_band       TEXT NOT NULL DEFAULT 'unresolved' CHECK (match_band IN ('high','medium','low','unresolved')),
  candidates_json  TEXT,
  reasons_json     TEXT,
  evidence_json    TEXT,
  flags_json       TEXT,
  new_product      INTEGER NOT NULL DEFAULT 0,
  corrected        INTEGER NOT NULL DEFAULT 0,
  po_item_id       TEXT,
  PRIMARY KEY (scan_id, line_no)
);
INSERT INTO invoice_scan_lines_new(scan_id, line_no, raw_text, description, code, qty_milli, unit_cost_minor, line_total_minor, product_id,
    match_kind, match_score, include, match_band)
  SELECT scan_id, line_no, raw_text, description, code, qty_milli, unit_cost_minor, line_total_minor, product_id, match_kind, match_score, include,
    CASE WHEN product_id IS NULL THEN 'unresolved' WHEN match_score >= 90 THEN 'high' WHEN match_score >= 70 THEN 'medium' ELSE 'low' END
  FROM invoice_scan_lines;
DROP TABLE invoice_scan_lines;
ALTER TABLE invoice_scan_lines_new RENAME TO invoice_scan_lines;

-- Confirmed supplier identities learned from reviews (deterministic memory).
CREATE TABLE supplier_aliases (
  alias_norm  TEXT NOT NULL,
  kind        TEXT NOT NULL CHECK (kind IN ('name','vat','cr','phone')),
  supplier_id TEXT NOT NULL REFERENCES suppliers(supplier_id),
  created_by  TEXT,
  created_at  TEXT NOT NULL,
  PRIMARY KEY (alias_norm, kind)
);

-- Confirmed supplier line → catalogue product mappings (by item code or by
-- normalized description), with the pack size staff confirmed.
CREATE TABLE supplier_product_map (
  supplier_id    TEXT NOT NULL REFERENCES suppliers(supplier_id),
  key_kind       TEXT NOT NULL CHECK (key_kind IN ('code','desc')),
  key_norm       TEXT NOT NULL,
  product_id     TEXT NOT NULL REFERENCES products(product_id),
  units_per_case INTEGER,
  uses           INTEGER NOT NULL DEFAULT 1,
  confirmed_by   TEXT,
  confirmed_at   TEXT NOT NULL,
  PRIMARY KEY (supplier_id, key_kind, key_norm)
);

-- Draft supplier invoices / credit notes created from reviewed documents.
-- AMWAPOS has no payables ledger: `approved` records the review only; no
-- liability, payment or stock follows from these rows.
CREATE TABLE supplier_invoices (
  invoice_id         TEXT PRIMARY KEY,
  number             TEXT NOT NULL UNIQUE,
  doc_type           TEXT NOT NULL CHECK (doc_type IN ('invoice','credit_note')),
  supplier_id        TEXT NOT NULL REFERENCES suppliers(supplier_id),
  scan_id            TEXT REFERENCES invoice_scans(scan_id),
  invoice_number     TEXT,
  invoice_date       TEXT,
  due_date           TEXT,
  po_id              TEXT,
  receiving_draft_id TEXT,
  subtotal_minor     INTEGER NOT NULL,
  vat_minor          INTEGER NOT NULL,
  total_minor        INTEGER NOT NULL,
  status             TEXT NOT NULL CHECK (status IN ('draft','approved','void')),
  posting            TEXT NOT NULL DEFAULT 'not_supported',
  notes              TEXT,
  created_by         TEXT NOT NULL,
  created_at         TEXT NOT NULL,
  updated_at         TEXT NOT NULL,
  approved_by        TEXT,
  approved_at        TEXT,
  revision           INTEGER NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX ux_supplier_invoices_scan ON supplier_invoices(scan_id) WHERE status <> 'void';
CREATE TABLE supplier_invoice_lines (
  invoice_id       TEXT NOT NULL REFERENCES supplier_invoices(invoice_id),
  line_no          INTEGER NOT NULL,
  product_id       TEXT,
  description      TEXT NOT NULL,
  qty_milli        INTEGER NOT NULL,
  unit_cost_minor  INTEGER NOT NULL,
  vat_rate_bp      INTEGER,
  vat_minor        INTEGER,
  line_total_minor INTEGER NOT NULL,
  PRIMARY KEY (invoice_id, line_no)
);

-- Draft receiving: posted only by a person through the normal receiving
-- (PO receive or direct receive), which writes the goods receipt and stock.
CREATE TABLE receiving_drafts (
  draft_id    TEXT PRIMARY KEY,
  number      TEXT NOT NULL UNIQUE,
  supplier_id TEXT NOT NULL REFERENCES suppliers(supplier_id),
  po_id       TEXT,
  scan_id     TEXT REFERENCES invoice_scans(scan_id),
  reference   TEXT,
  status      TEXT NOT NULL CHECK (status IN ('draft','posted','cancelled')),
  receipt_ids TEXT,
  created_by  TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  posted_by   TEXT,
  posted_at   TEXT,
  revision    INTEGER NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX ux_receiving_drafts_scan ON receiving_drafts(scan_id) WHERE status <> 'cancelled';
CREATE TABLE receiving_draft_lines (
  draft_id        TEXT NOT NULL REFERENCES receiving_drafts(draft_id),
  line_no         INTEGER NOT NULL,
  product_id      TEXT NOT NULL REFERENCES products(product_id),
  description     TEXT NOT NULL,
  qty_milli       INTEGER NOT NULL CHECK (qty_milli > 0),
  unit_cost_minor INTEGER NOT NULL CHECK (unit_cost_minor >= 0),
  case_qty_milli  INTEGER,
  units_per_case  INTEGER,
  po_item_id      TEXT,
  scan_line_no    INTEGER,
  PRIMARY KEY (draft_id, line_no)
);

-- Structured decision trail for AI-assisted work (no model reasoning text).
CREATE TABLE ai_decisions (
  decision_id TEXT PRIMARY KEY,
  subject     TEXT NOT NULL,
  subject_id  TEXT NOT NULL,
  kind        TEXT NOT NULL,
  source      TEXT NOT NULL CHECK (source IN ('rules','ai','person','system')),
  model       TEXT,
  data_json   TEXT NOT NULL,
  user_id     TEXT,
  created_at  TEXT NOT NULL
);
CREATE INDEX ix_ai_decisions_subject ON ai_decisions(subject, subject_id, created_at);
