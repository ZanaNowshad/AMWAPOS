-- Accounts Payable foundation.
--
-- Separation of concerns:
--   invoice_scans          source document + OCR / extraction (never money truth)
--   supplier_invoices      reviewable business record (draft → approved)
--   ap_liabilities         what AMWAPOS owes, created ONLY by an explicit,
--                          authorized posting of an approved invoice
--   ap_credits             posted supplier credit notes
--   ap_payments            money paid to a supplier
--   ap_allocations         which payment / credit settles which liability
--
-- Balances are never stored: a supplier's balance is posted liabilities minus
-- posted credits minus posted payments, and an invoice's outstanding amount
-- is its liability minus its active allocations. Posted records are never
-- edited or deleted; they are reversed, which is itself recorded.
--
-- This migration changes no money, stock or message: existing supplier
-- invoice records keep their review status and are marked "not posted".

-- supplier_invoices.posting: not_posted | posted | reversed (was the constant 'not_supported').
UPDATE supplier_invoices SET posting = 'not_posted' WHERE posting = 'not_supported' OR posting IS NULL OR posting = '';
ALTER TABLE supplier_invoices ADD COLUMN currency TEXT NOT NULL DEFAULT 'BHD';
ALTER TABLE supplier_invoices ADD COLUMN source TEXT NOT NULL DEFAULT 'document';
ALTER TABLE supplier_invoices ADD COLUMN applies_to_invoice_id TEXT;
ALTER TABLE supplier_invoices ADD COLUMN posted_at TEXT;
ALTER TABLE supplier_invoices ADD COLUMN posted_by TEXT;
ALTER TABLE supplier_invoices ADD COLUMN reversed_at TEXT;
ALTER TABLE supplier_invoices ADD COLUMN reversed_by TEXT;
ALTER TABLE supplier_invoices ADD COLUMN reversal_reason TEXT;
CREATE INDEX ix_supplier_invoices_supplier ON supplier_invoices(supplier_id, invoice_date);
CREATE INDEX ix_supplier_invoices_number ON supplier_invoices(supplier_id, invoice_number);

-- A posted invoice is immutable: amounts, supplier, number, dates and lines.
CREATE TRIGGER trg_supplier_invoices_posted_immutable BEFORE UPDATE OF supplier_id, doc_type, invoice_number, invoice_date, due_date,
    subtotal_minor, vat_minor, total_minor, currency ON supplier_invoices
  WHEN OLD.posting IN ('posted','reversed')
  BEGIN SELECT RAISE(ABORT, 'a posted supplier invoice cannot be edited; reverse it instead'); END;
CREATE TRIGGER trg_supplier_invoices_no_delete_posted BEFORE DELETE ON supplier_invoices
  WHEN OLD.posting IN ('posted','reversed')
  BEGIN SELECT RAISE(ABORT, 'a posted supplier invoice cannot be deleted'); END;
CREATE TRIGGER trg_supplier_invoice_lines_posted_upd BEFORE UPDATE ON supplier_invoice_lines
  WHEN (SELECT posting FROM supplier_invoices WHERE invoice_id = OLD.invoice_id) IN ('posted','reversed')
  BEGIN SELECT RAISE(ABORT, 'lines of a posted supplier invoice cannot be edited'); END;
CREATE TRIGGER trg_supplier_invoice_lines_posted_del BEFORE DELETE ON supplier_invoice_lines
  WHEN (SELECT posting FROM supplier_invoices WHERE invoice_id = OLD.invoice_id) IN ('posted','reversed')
  BEGIN SELECT RAISE(ABORT, 'lines of a posted supplier invoice cannot be deleted'); END;
CREATE TRIGGER trg_supplier_invoice_lines_posted_ins BEFORE INSERT ON supplier_invoice_lines
  WHEN (SELECT posting FROM supplier_invoices WHERE invoice_id = NEW.invoice_id) IN ('posted','reversed')
  BEGIN SELECT RAISE(ABORT, 'lines cannot be added to a posted supplier invoice'); END;

CREATE TABLE ap_liabilities (
  liability_id     TEXT PRIMARY KEY,
  supplier_id      TEXT NOT NULL REFERENCES suppliers(supplier_id),
  -- One liability per invoice: a second posting is impossible.
  invoice_id       TEXT NOT NULL UNIQUE REFERENCES supplier_invoices(invoice_id),
  doc_date         TEXT NOT NULL,
  due_date         TEXT NOT NULL,
  due_rule         TEXT NOT NULL CHECK (due_rule IN ('invoice','terms','invoice_date')),
  amount_minor     INTEGER NOT NULL CHECK (amount_minor > 0),
  currency         TEXT NOT NULL DEFAULT 'BHD',
  status           TEXT NOT NULL CHECK (status IN ('open','reversed')),
  operation_id     TEXT NOT NULL UNIQUE,
  posted_by        TEXT NOT NULL,
  posted_at        TEXT NOT NULL,
  reversed_by      TEXT,
  reversed_at      TEXT,
  reversal_reason  TEXT
);
CREATE INDEX ix_ap_liabilities_supplier ON ap_liabilities(supplier_id, status, due_date);

CREATE TABLE ap_credits (
  credit_id        TEXT PRIMARY KEY,
  supplier_id      TEXT NOT NULL REFERENCES suppliers(supplier_id),
  -- The credit note's own record (doc_type credit_note), one credit per note.
  invoice_id       TEXT NOT NULL UNIQUE REFERENCES supplier_invoices(invoice_id),
  -- The invoice it corrects, when known.
  applies_to_invoice_id TEXT REFERENCES supplier_invoices(invoice_id),
  doc_date         TEXT NOT NULL,
  amount_minor     INTEGER NOT NULL CHECK (amount_minor > 0),
  currency         TEXT NOT NULL DEFAULT 'BHD',
  status           TEXT NOT NULL CHECK (status IN ('open','reversed')),
  operation_id     TEXT NOT NULL UNIQUE,
  posted_by        TEXT NOT NULL,
  posted_at        TEXT NOT NULL,
  reversed_by      TEXT,
  reversed_at      TEXT,
  reversal_reason  TEXT
);
CREATE INDEX ix_ap_credits_supplier ON ap_credits(supplier_id, status);

CREATE TABLE ap_payments (
  payment_id       TEXT PRIMARY KEY,
  number           TEXT NOT NULL UNIQUE,
  supplier_id      TEXT NOT NULL REFERENCES suppliers(supplier_id),
  paid_on          TEXT NOT NULL,
  amount_minor     INTEGER NOT NULL CHECK (amount_minor > 0),
  currency         TEXT NOT NULL DEFAULT 'BHD',
  method           TEXT NOT NULL CHECK (method IN ('cash','bank_transfer','cheque','card','benefitpay','other')),
  reference        TEXT,
  notes            TEXT,
  status           TEXT NOT NULL CHECK (status IN ('posted','reversed')),
  operation_id     TEXT NOT NULL UNIQUE,
  created_by       TEXT NOT NULL,
  created_at       TEXT NOT NULL,
  reversed_by      TEXT,
  reversed_at      TEXT,
  reversal_reason  TEXT
);
CREATE INDEX ix_ap_payments_supplier ON ap_payments(supplier_id, status, paid_on);

CREATE TABLE ap_allocations (
  allocation_id    TEXT PRIMARY KEY,
  kind             TEXT NOT NULL CHECK (kind IN ('payment','credit')),
  payment_id       TEXT REFERENCES ap_payments(payment_id),
  credit_id        TEXT REFERENCES ap_credits(credit_id),
  liability_id     TEXT NOT NULL REFERENCES ap_liabilities(liability_id),
  amount_minor     INTEGER NOT NULL CHECK (amount_minor > 0),
  status           TEXT NOT NULL CHECK (status IN ('active','reversed')),
  created_by       TEXT NOT NULL,
  created_at       TEXT NOT NULL,
  reversed_by      TEXT,
  reversed_at      TEXT,
  CHECK ((kind = 'payment' AND payment_id IS NOT NULL AND credit_id IS NULL)
      OR (kind = 'credit' AND credit_id IS NOT NULL AND payment_id IS NULL))
);
CREATE INDEX ix_ap_allocations_liability ON ap_allocations(liability_id, status);
CREATE INDEX ix_ap_allocations_payment ON ap_allocations(payment_id, status);
CREATE INDEX ix_ap_allocations_credit ON ap_allocations(credit_id, status);

-- Posted accounting records are never deleted and their amounts never change.
CREATE TRIGGER trg_ap_liabilities_no_delete BEFORE DELETE ON ap_liabilities BEGIN SELECT RAISE(ABORT, 'posted accounting records are never deleted'); END;
CREATE TRIGGER trg_ap_credits_no_delete BEFORE DELETE ON ap_credits BEGIN SELECT RAISE(ABORT, 'posted accounting records are never deleted'); END;
CREATE TRIGGER trg_ap_payments_no_delete BEFORE DELETE ON ap_payments BEGIN SELECT RAISE(ABORT, 'posted accounting records are never deleted'); END;
CREATE TRIGGER trg_ap_allocations_no_delete BEFORE DELETE ON ap_allocations BEGIN SELECT RAISE(ABORT, 'posted accounting records are never deleted'); END;
CREATE TRIGGER trg_ap_liabilities_fixed BEFORE UPDATE OF supplier_id, invoice_id, doc_date, amount_minor, currency, operation_id, posted_by, posted_at ON ap_liabilities
  BEGIN SELECT RAISE(ABORT, 'posted accounting records are never edited; reverse them instead'); END;
CREATE TRIGGER trg_ap_credits_fixed BEFORE UPDATE OF supplier_id, invoice_id, doc_date, amount_minor, currency, operation_id, posted_by, posted_at ON ap_credits
  BEGIN SELECT RAISE(ABORT, 'posted accounting records are never edited; reverse them instead'); END;
CREATE TRIGGER trg_ap_payments_fixed BEFORE UPDATE OF supplier_id, paid_on, amount_minor, currency, method, operation_id, created_by, created_at ON ap_payments
  BEGIN SELECT RAISE(ABORT, 'posted accounting records are never edited; reverse them instead'); END;
CREATE TRIGGER trg_ap_allocations_fixed BEFORE UPDATE OF kind, payment_id, credit_id, liability_id, amount_minor, created_by, created_at ON ap_allocations
  BEGIN SELECT RAISE(ABORT, 'posted accounting records are never edited; reverse them instead'); END;
-- A reversed record stays reversed.
CREATE TRIGGER trg_ap_liabilities_no_reopen BEFORE UPDATE OF status ON ap_liabilities WHEN OLD.status = 'reversed'
  BEGIN SELECT RAISE(ABORT, 'a reversed record cannot be reopened'); END;
CREATE TRIGGER trg_ap_credits_no_reopen BEFORE UPDATE OF status ON ap_credits WHEN OLD.status = 'reversed'
  BEGIN SELECT RAISE(ABORT, 'a reversed record cannot be reopened'); END;
CREATE TRIGGER trg_ap_payments_no_reopen BEFORE UPDATE OF status ON ap_payments WHEN OLD.status = 'reversed'
  BEGIN SELECT RAISE(ABORT, 'a reversed record cannot be reopened'); END;
CREATE TRIGGER trg_ap_allocations_no_reopen BEFORE UPDATE OF status ON ap_allocations WHEN OLD.status = 'reversed'
  BEGIN SELECT RAISE(ABORT, 'a reversed record cannot be reopened'); END;

-- Canonical purchase-cost history. product_cost_history (append-only, synced)
-- already records every receiving cost; posted supplier invoices append to
-- it too (source 'supplier_invoice', effective at the invoice date). This
-- view adds the supplier, date and quantity and leaves out reversed
-- invoices. Document Intelligence reads it; it owns no history of its own.
CREATE VIEW purchase_cost_history AS
SELECT h.cost_id, h.product_id, h.supplier_id, h.source, h.source_id, h.effective_at,
       substr(h.effective_at, 1, 10) AS doc_date,
       h.cost_minor AS unit_cost_minor,
       CASE h.source
         WHEN 'receiving' THEN (SELECT SUM(i.qty_milli) FROM goods_receipt_items i WHERE i.receipt_id = h.source_id AND i.product_id = h.product_id)
         WHEN 'supplier_invoice' THEN (SELECT SUM(l.qty_milli) FROM supplier_invoice_lines l WHERE l.invoice_id = h.source_id AND l.product_id = h.product_id)
       END AS qty_milli
FROM product_cost_history h
WHERE h.source IN ('receiving', 'supplier_invoice')
  AND NOT (h.source = 'supplier_invoice'
           AND EXISTS (SELECT 1 FROM supplier_invoices s WHERE s.invoice_id = h.source_id AND s.posting = 'reversed'));
CREATE INDEX ix_cost_supplier ON product_cost_history(supplier_id, product_id, effective_at);

-- Supplier-document profiles: confirmed recurring patterns (hints only).
CREATE TABLE supplier_doc_profiles (
  supplier_id      TEXT NOT NULL REFERENCES suppliers(supplier_id),
  kind             TEXT NOT NULL CHECK (kind IN ('invoice_number_label','invoice_number_shape','sku_column','pack_style','ocr_fix','header_hint')),
  value            TEXT NOT NULL,
  uses             INTEGER NOT NULL DEFAULT 1,
  confirmed_by     TEXT,
  first_seen_scan  TEXT,
  last_seen_at     TEXT NOT NULL,
  PRIMARY KEY (supplier_id, kind, value)
);
