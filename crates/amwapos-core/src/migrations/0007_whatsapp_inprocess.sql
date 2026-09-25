-- WhatsApp moves in-process (whatsapp-rust adapter; no Node sidecar).
--  * wa_outbox: more message kinds (delivered, payment_ack, document) and a
--    payload hash so the same send key with a different payload is refused.
--  * wa_inbox: inbound media is recorded first and downloaded later by the
--    WhatsApp service (media_ref holds the encrypted download parameters).
--  * payment_reviews: OCR outcome is ocr_match | likely_match | mismatch |
--    needs_review; confirmed / rejected remain a person's decision.
-- WhatsApp's own session (keys, device identity) is NOT in this database: it
-- lives in <data>/whatsapp/session.db, opened only by the WhatsApp service.

CREATE TABLE wa_outbox_new (
  message_id     TEXT PRIMARY KEY,
  operation_id   TEXT NOT NULL UNIQUE,
  payload_hash   TEXT NOT NULL DEFAULT '',
  kind           TEXT NOT NULL CHECK (kind IN ('receipt','dispatch','delivered','reminder','payment_ack','text','document')),
  to_phone       TEXT NOT NULL,
  customer_id    TEXT,
  sale_id        TEXT,
  delivery_id    TEXT,
  review_id      TEXT,
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
INSERT INTO wa_outbox_new(message_id, operation_id, kind, to_phone, customer_id, sale_id, delivery_id, lang, body, document_path,
    document_name, status, attempts, last_error, wa_message_id, created_by, created_at, updated_at, sent_at, next_attempt_at)
  SELECT message_id, operation_id, kind, to_phone, customer_id, sale_id, delivery_id, lang, body, document_path,
    document_name, status, attempts, last_error, wa_message_id, created_by, created_at, updated_at, sent_at, next_attempt_at
  FROM wa_outbox;
DROP TABLE wa_outbox;
ALTER TABLE wa_outbox_new RENAME TO wa_outbox;
CREATE INDEX ix_wa_outbox_status ON wa_outbox(status, next_attempt_at);
CREATE INDEX ix_wa_outbox_sale ON wa_outbox(sale_id);

ALTER TABLE wa_inbox ADD COLUMN media_ref TEXT;
ALTER TABLE wa_inbox ADD COLUMN media_state TEXT NOT NULL DEFAULT 'none'
  CHECK (media_state IN ('none','pending','saved','failed'));
ALTER TABLE wa_inbox ADD COLUMN media_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE wa_inbox ADD COLUMN media_error TEXT;
UPDATE wa_inbox SET media_state='saved' WHERE media_path IS NOT NULL;
CREATE INDEX ix_wa_inbox_media ON wa_inbox(media_state);

CREATE TABLE payment_reviews_new (
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
  -- OCR assessment, kept after a person decides.
  ocr_status         TEXT CHECK (ocr_status IN ('ocr_match','likely_match','mismatch','needs_review')),
  status             TEXT NOT NULL CHECK (status IN ('pending','ocr_match','likely_match','mismatch','needs_review','confirmed','rejected')),
  reason             TEXT,
  decided_by         TEXT,
  decided_at         TEXT,
  note               TEXT,
  created_at         TEXT NOT NULL,
  updated_at         TEXT NOT NULL
);
INSERT INTO payment_reviews_new(review_id, review_number, source, inbox_seq, image_path, image_sha256, phone, customer_id, sale_id, delivery_id,
    expected_minor, ocr_text, ocr_confidence, detected_minor, detected_reference, duplicate_of, ocr_status, status, reason, decided_by,
    decided_at, note, created_at, updated_at)
  SELECT review_id, review_number, source, inbox_seq, image_path, image_sha256, phone, customer_id, sale_id, delivery_id,
    expected_minor, ocr_text, ocr_confidence, detected_minor, detected_reference, duplicate_of,
    CASE WHEN status='matched' THEN 'ocr_match' WHEN status IN ('mismatch','needs_review') THEN status
         WHEN ocr_text IS NOT NULL THEN 'needs_review' ELSE NULL END,
    CASE WHEN status='matched' THEN 'ocr_match' ELSE status END,
    reason, decided_by, decided_at, note, created_at, updated_at
  FROM payment_reviews;
DROP TABLE payment_reviews;
ALTER TABLE payment_reviews_new RENAME TO payment_reviews;
CREATE INDEX ix_payment_reviews_status ON payment_reviews(status, created_at);
CREATE INDEX ix_payment_reviews_sha ON payment_reviews(image_sha256);

-- The sidecar inbox cursor is gone (inbound messages are committed directly).
DELETE FROM settings WHERE key='local.whatsapp_cursor';
