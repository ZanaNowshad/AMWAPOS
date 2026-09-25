-- Invoice scans record which parser produced the lines (rules | ai), and
-- PDF receipt copies that failed after commit are retried from a queue.

ALTER TABLE invoice_scans ADD COLUMN parser TEXT NOT NULL DEFAULT 'rules' CHECK (parser IN ('rules','ai'));

CREATE TABLE pdf_receipt_queue (
  kind        TEXT NOT NULL CHECK (kind IN ('sale','refund')),
  ref_id      TEXT NOT NULL,
  attempts    INTEGER NOT NULL DEFAULT 0,
  last_error  TEXT,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  PRIMARY KEY (kind, ref_id)
);
