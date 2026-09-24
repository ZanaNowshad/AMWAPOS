-- WhatsApp messages, payment screenshot reviews and invoice scans.
-- These records stay on the computer that runs the WhatsApp/OCR sidecar
-- (normally the hub); they are not synced to terminals.

CREATE TABLE wa_outbox (
  message_id     TEXT PRIMARY KEY,          -- also the sidecar client_id (send once)
  operation_id   TEXT NOT NULL UNIQUE,
  kind           TEXT NOT NULL CHECK (kind IN ('receipt','dispatch','reminder','text')),
  to_phone       TEXT NOT NULL,
  customer_id    TEXT,
  sale_id        TEXT,
  delivery_id    TEXT,
  lang           TEXT NOT NULL CHECK (lang IN ('en','ar')),
  body           TEXT NOT NULL,
  document_path  TEXT,
  document_name  TEXT,
  status         TEXT NOT NULL CHECK (status IN ('queued','sending','sent','failed','cancelled')),
  attempts       INTEGER NOT NULL DEFAULT 0,
  last_error     TEXT,
  wa_message_id  TEXT,
  created_by     TEXT NOT NULL,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL,
  sent_at        TEXT,
  next_attempt_at TEXT
);
CREATE INDEX ix_wa_outbox_status ON wa_outbox(status, next_attempt_at);

CREATE TABLE wa_inbox (
  seq          INTEGER PRIMARY KEY AUTOINCREMENT,
  wa_id        TEXT NOT NULL,
  chat         TEXT NOT NULL,
  phone        TEXT,
  push_name    TEXT,
  received_at  TEXT NOT NULL,
  kind         TEXT NOT NULL CHECK (kind IN ('text','image','document','other')),
  body         TEXT,
  caption      TEXT,
  media_path   TEXT,
  media_mime   TEXT,
  media_sha256 TEXT,
  customer_id  TEXT,
  read_at      TEXT,
  read_synced  INTEGER NOT NULL DEFAULT 0,
  UNIQUE (chat, wa_id)
);
CREATE INDEX ix_wa_inbox_chat ON wa_inbox(chat, seq);

CREATE TABLE payment_reviews (
  review_id          TEXT PRIMARY KEY,
  review_number      TEXT NOT NULL UNIQUE,
  source             TEXT NOT NULL CHECK (source IN ('whatsapp','upload')),
  inbox_seq          INTEGER,
  image_path         TEXT NOT NULL,
  image_sha256       TEXT NOT NULL,
  phone              TEXT,
  customer_id        TEXT,
  sale_id            TEXT,
  delivery_id        TEXT,
  expected_minor     INTEGER,
  ocr_text           TEXT,
  ocr_confidence     INTEGER,
  detected_minor     INTEGER,
  detected_reference TEXT,
  duplicate_of       TEXT,
  status             TEXT NOT NULL CHECK (status IN ('pending','matched','mismatch','needs_review','confirmed','rejected')),
  reason             TEXT,
  decided_by         TEXT,
  decided_at         TEXT,
  note               TEXT,
  created_at         TEXT NOT NULL,
  updated_at         TEXT NOT NULL
);
CREATE INDEX ix_payment_reviews_status ON payment_reviews(status, created_at);
CREATE INDEX ix_payment_reviews_sha ON payment_reviews(image_sha256);

CREATE TABLE invoice_scans (
  scan_id        TEXT PRIMARY KEY,
  scan_number    TEXT NOT NULL UNIQUE,
  supplier_id    TEXT,
  image_path     TEXT NOT NULL,
  image_sha256   TEXT NOT NULL,
  file_name      TEXT,
  status         TEXT NOT NULL CHECK (status IN ('imported','read','review','confirmed','rejected','failed')),
  ocr_text       TEXT,
  ocr_confidence INTEGER,
  invoice_number TEXT,
  invoice_date   TEXT,
  total_minor    INTEGER,
  po_id          TEXT,
  error          TEXT,
  created_by     TEXT NOT NULL,
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL
);
CREATE INDEX ix_invoice_scans_status ON invoice_scans(status, created_at);

CREATE TABLE invoice_scan_lines (
  scan_id          TEXT NOT NULL REFERENCES invoice_scans(scan_id),
  line_no          INTEGER NOT NULL,
  raw_text         TEXT NOT NULL,
  description      TEXT,
  code             TEXT,
  qty_milli        INTEGER,
  unit_cost_minor  INTEGER,
  line_total_minor INTEGER,
  product_id       TEXT,
  match_kind       TEXT NOT NULL CHECK (match_kind IN ('barcode','sku','name','manual','none')),
  match_score      INTEGER NOT NULL DEFAULT 0,
  include          INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (scan_id, line_no)
);
