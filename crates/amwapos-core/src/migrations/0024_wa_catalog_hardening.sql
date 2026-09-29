-- WhatsApp catalogue hardening (audit 2026-09-29). Adds state only; nothing is
-- queued or published by this migration.
--
-- image_failed_hash   a POS picture WhatsApp (or the local check) refused:
--                     the product is published without it until the picture
--                     changes, instead of failing the whole product.
-- pending_image_*     a picture uploaded for a write that then failed: reused
--                     by the retry for a day instead of uploading it again.
-- price_1000          last price published (thousandths): a product archived
--                     after losing its price is hidden with this price.
-- run_id              the administrator's full sync this row belongs to, for
--                     progress and for its explicit "republish what was
--                     deleted on WhatsApp" policy; cleared when the row is done.
ALTER TABLE wa_catalog_products ADD COLUMN image_failed_hash TEXT;
ALTER TABLE wa_catalog_products ADD COLUMN pending_image_hash TEXT;
ALTER TABLE wa_catalog_products ADD COLUMN pending_image_url TEXT;
ALTER TABLE wa_catalog_products ADD COLUMN pending_image_at TEXT;
ALTER TABLE wa_catalog_products ADD COLUMN price_1000 INTEGER;
ALTER TABLE wa_catalog_products ADD COLUMN run_id TEXT;
CREATE INDEX ix_wa_catalog_run ON wa_catalog_products(account, run_id) WHERE run_id IS NOT NULL;

-- One row per full sync an administrator started (per linked account).
-- verify: pending → the worker compares the remote catalogue once and
-- re-checks mapped products that are not listed there (done | skipped).
CREATE TABLE wa_catalog_runs (
  run_id       TEXT PRIMARY KEY,
  account      TEXT NOT NULL,
  started_at   TEXT NOT NULL,
  started_by   TEXT,
  total        INTEGER NOT NULL DEFAULT 0,
  unchanged    INTEGER NOT NULL DEFAULT 0,
  done_synced  INTEGER NOT NULL DEFAULT 0,
  done_hidden  INTEGER NOT NULL DEFAULT 0,
  done_removed INTEGER NOT NULL DEFAULT 0,
  done_failed  INTEGER NOT NULL DEFAULT 0,
  verify       TEXT NOT NULL DEFAULT 'pending' CHECK (verify IN ('pending','done','skipped')),
  verify_note  TEXT,
  finished_at  TEXT
);
-- At most one unfinished run per account.
CREATE UNIQUE INDEX ux_wa_catalog_run_open ON wa_catalog_runs(account) WHERE finished_at IS NULL;
