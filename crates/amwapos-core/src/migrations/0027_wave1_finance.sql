-- Wave 1 of the merchant operating system (docs/MERCHANT_OS_PLAN.md).

-- The receipt exactly as issued: the rendered document (header, lines,
-- totals, footer) as canonical JSON, written in the same transaction as the
-- sale or refund, with its SHA-256. Reprints come from here, so later changes
-- to the business name, VAT number, address, header, footer or receipt
-- language never change an issued receipt. Records made before this
-- migration have no snapshot and reprint as reconstructed.
CREATE TABLE receipt_snapshots (
  ref_kind       TEXT NOT NULL CHECK (ref_kind IN ('sale','refund','void')),
  ref_id         TEXT NOT NULL,
  format_version INTEGER NOT NULL,
  doc_json       TEXT NOT NULL,
  copy_at        INTEGER NOT NULL,
  sha256         TEXT NOT NULL,
  created_at     TEXT NOT NULL,
  PRIMARY KEY (ref_kind, ref_id)
);
CREATE TRIGGER trg_receipt_snapshots_no_update BEFORE UPDATE ON receipt_snapshots BEGIN SELECT RAISE(ABORT, 'issued receipts are immutable'); END;
CREATE TRIGGER trg_receipt_snapshots_no_delete BEFORE DELETE ON receipt_snapshots BEGIN SELECT RAISE(ABORT, 'issued receipts are immutable'); END;
CREATE TRIGGER trg_sync_receipt_snapshots_insert AFTER INSERT ON receipt_snapshots WHEN (SELECT v FROM sync_control WHERE k='suppress') IS NOT '1'
BEGIN
  INSERT INTO sync_outbox(table_name, row_pk, op, origin) VALUES ('receipt_snapshots', json_object('ref_kind', NEW.ref_kind, 'ref_id', NEW.ref_id), 'upsert', (SELECT v FROM sync_control WHERE k='origin'));
END;
