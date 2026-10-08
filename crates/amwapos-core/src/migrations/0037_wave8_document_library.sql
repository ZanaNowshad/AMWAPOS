-- Wave 8: Document Library (docs/INTELLIGENCE_AND_EVIDENCE.md).
--
-- A file is stored once, by the SHA-256 of its bytes (`library_files`). A
-- document is a business record about a file: its title, category, the
-- records it is evidence for (`library_links`) and who added it. Identical
-- bytes are one document, linked to every record they are evidence for.
--
-- Nothing here copies a financial field: an invoice document links to the
-- supplier invoice, which holds the amounts. Dates and titles are entered by
-- people; nothing is guessed.
--
-- A document's file never changes. Replacing it adds a new version; the old
-- one stays, with its file and hash, marked as replaced. Evidence that was
-- ever linked to a record is archived, never deleted.
--
-- All tables are hub-local (sync::LOCAL_TABLES). `library_fts` is an index
-- rebuilt from `library_documents` and `library_file_pages` at any time.

CREATE TABLE library_files (
  sha256      TEXT PRIMARY KEY CHECK (length(sha256) = 64),
  -- Relative to the data folder when the file is inside it.
  path        TEXT NOT NULL,
  mime        TEXT NOT NULL,
  -- NULL for files adopted from earlier records whose size was not kept.
  bytes       INTEGER CHECK (bytes IS NULL OR bytes > 0),
  page_count  INTEGER,
  -- pending: not read yet; extracted: text kept in library_file_pages;
  -- none: the file has no readable text; failed: reading it failed.
  text_status TEXT NOT NULL DEFAULT 'pending' CHECK (text_status IN ('pending','extracted','none','failed')),
  text_source TEXT CHECK (text_source IS NULL OR text_source IN ('pdf_text','ocr')),
  text_note   TEXT,
  created_at  TEXT NOT NULL
);
CREATE INDEX ix_library_files_pending ON library_files(text_status) WHERE text_status = 'pending';

-- Text read from a file, per page. Page 0 = the page is not known (text read
-- from the whole file at once); citations show a page only when it is known.
CREATE TABLE library_file_pages (
  sha256 TEXT NOT NULL REFERENCES library_files(sha256),
  page   INTEGER NOT NULL CHECK (page >= 0),
  text   TEXT NOT NULL,
  PRIMARY KEY (sha256, page)
);

CREATE TABLE library_documents (
  document_id   TEXT PRIMARY KEY,
  -- Sequence for the number (DOC-000001) and the search index row ids.
  seq           INTEGER NOT NULL UNIQUE CHECK (seq > 0),
  number        TEXT NOT NULL UNIQUE,
  title         TEXT NOT NULL,
  category      TEXT NOT NULL CHECK (category IN (
                  'invoice','credit_note','receipt','delivery_note','quotation','price_list',
                  'statement','contract','licence','insurance','bank','tax','other')),
  sha256        TEXT NOT NULL REFERENCES library_files(sha256),
  original_name TEXT NOT NULL,
  -- Entered by a person (the date printed on it); never guessed.
  document_date TEXT,
  note          TEXT,
  version       INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
  replaces_id   TEXT REFERENCES library_documents(document_id),
  replaced_by   TEXT REFERENCES library_documents(document_id),
  status        TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','replaced','archived')),
  -- Where it came from: added in the library, or an earlier record's file.
  source        TEXT NOT NULL DEFAULT 'upload' CHECK (source IN ('upload','expense_attachment','invoice_scan','case_evidence')),
  branch_id     TEXT,
  added_by      TEXT NOT NULL,
  added_at      TEXT NOT NULL,
  archived_by   TEXT,
  archived_at   TEXT,
  archive_reason TEXT,
  updated_at    TEXT NOT NULL
);
CREATE INDEX ix_library_documents_status ON library_documents(status, added_at);
CREATE INDEX ix_library_documents_sha ON library_documents(sha256);
CREATE INDEX ix_library_documents_category ON library_documents(category, status);

-- The file, its origin and its place in the version chain never change.
CREATE TRIGGER trg_library_documents_fixed
BEFORE UPDATE OF sha256, original_name, seq, number, version, replaces_id, source, added_by, added_at ON library_documents
BEGIN SELECT RAISE(ABORT, 'a document''s file cannot be changed; add a new version'); END;

-- A replaced version stays replaced.
CREATE TRIGGER trg_library_documents_replaced
BEFORE UPDATE OF status, replaced_by ON library_documents
WHEN OLD.replaced_by IS NOT NULL AND (NEW.replaced_by IS NOT OLD.replaced_by OR NEW.status <> OLD.status)
BEGIN SELECT RAISE(ABORT, 'a replaced version cannot be changed'); END;

CREATE TABLE library_links (
  link_id     TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES library_documents(document_id),
  entity_type TEXT NOT NULL CHECK (entity_type IN (
                'supplier','supplier_invoice','purchase_order','expense','product','case',
                'day_close','supplier_return','requisition','customer','promotion')),
  entity_id   TEXT NOT NULL,
  linked_by   TEXT NOT NULL,
  linked_at   TEXT NOT NULL,
  -- A removed link is kept (who removed it, when): it was once evidence.
  removed_by  TEXT,
  removed_at  TEXT
);
CREATE UNIQUE INDEX ux_library_links_active ON library_links(document_id, entity_type, entity_id) WHERE removed_at IS NULL;
CREATE INDEX ix_library_links_entity ON library_links(entity_type, entity_id) WHERE removed_at IS NULL;
CREATE INDEX ix_library_links_document ON library_links(document_id);

CREATE TRIGGER trg_library_links_no_delete BEFORE DELETE ON library_links
BEGIN SELECT RAISE(ABORT, 'a link is removed, not deleted'); END;

-- Evidence that was ever linked, came from an earlier record or is part of
-- a version chain is archived, never deleted.
CREATE TRIGGER trg_library_documents_no_delete BEFORE DELETE ON library_documents
WHEN OLD.source <> 'upload' OR OLD.replaces_id IS NOT NULL OR OLD.replaced_by IS NOT NULL
  OR EXISTS (SELECT 1 FROM library_links l WHERE l.document_id = OLD.document_id)
BEGIN SELECT RAISE(ABORT, 'linked evidence is archived, not deleted'); END;

-- Earlier records whose files were adopted into the library (once each).
CREATE TABLE library_adoptions (
  source      TEXT NOT NULL CHECK (source IN ('expense_attachment','invoice_scan','case_evidence')),
  source_ref  TEXT NOT NULL,
  document_id TEXT NOT NULL REFERENCES library_documents(document_id),
  adopted_at  TEXT NOT NULL,
  PRIMARY KEY (source, source_ref)
);

-- Search index: one row per document page (page 0 = title only, or text whose
-- page is not known). Row id = seq * 10000 + page, so a document's rows are
-- found by range. Rebuilt by `library::reindex`.
CREATE VIRTUAL TABLE library_fts USING fts5(
  title,
  body,
  tokenize = 'unicode61 remove_diacritics 2',
  prefix = '2 3 4'
);
