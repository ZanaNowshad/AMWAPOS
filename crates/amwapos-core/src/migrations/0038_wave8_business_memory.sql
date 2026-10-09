-- Wave 8: Business Memory (docs/INTELLIGENCE_AND_EVIDENCE.md).
--
-- Things about this business that are true but live in no record: "Gulf
-- Dairy delivers on Sundays and Wednesdays", "The landlord wants the rent by
-- bank transfer before the 5th". Each is one statement with where it came
-- from and who confirmed it.
--
-- Lifecycle: candidate -> confirmed -> superseded | archived; a candidate can
-- be rejected. The assistant only adds candidates; a person confirms. Changing
-- a confirmed statement adds a new one that supersedes it; the old stays.
--
-- Memory never overrides a record: where a record says otherwise, the record
-- is right and the memory is shown as possibly outdated. Memory is never an
-- authorisation for anything.
--
-- The table starts empty: nothing is inferred from existing data.
-- Hub-local (sync::LOCAL_TABLES); memory_fts is rebuilt from it at any time.

CREATE TABLE business_memories (
  memory_id      TEXT PRIMARY KEY,
  seq            INTEGER NOT NULL UNIQUE CHECK (seq > 0),
  number         TEXT NOT NULL UNIQUE,
  statement      TEXT NOT NULL CHECK (length(statement) BETWEEN 3 AND 500),
  status         TEXT NOT NULL CHECK (status IN ('candidate','confirmed','superseded','archived','rejected')),
  -- business: everywhere; branch: one branch (branch_id).
  scope          TEXT NOT NULL DEFAULT 'business' CHECK (scope IN ('business','branch')),
  branch_id      TEXT,
  -- The record it is about (optional): the same kinds as library links.
  entity_type    TEXT CHECK (entity_type IS NULL OR entity_type IN (
                   'supplier','supplier_invoice','purchase_order','expense','product','case',
                   'day_close','supplier_return','requisition','customer','promotion')),
  entity_id      TEXT,
  -- Where it came from.
  source_kind    TEXT NOT NULL CHECK (source_kind IN ('person','assistant','document')),
  source_ref     TEXT,
  -- The words it was taken from (assistant: the person's message; document:
  -- the passage), bounded. Shown to people; DATA to the assistant.
  source_excerpt TEXT,
  -- Suggested after the conversation had read text from outside the store.
  from_untrusted INTEGER NOT NULL DEFAULT 0 CHECK (from_untrusted IN (0,1)),
  proposed_by    TEXT NOT NULL,
  proposed_at    TEXT NOT NULL,
  confirmed_by   TEXT,
  confirmed_at   TEXT,
  last_verified_at TEXT,
  valid_until    TEXT,
  supersedes     TEXT REFERENCES business_memories(memory_id),
  superseded_by  TEXT REFERENCES business_memories(memory_id),
  decided_by     TEXT,
  decided_at     TEXT,
  decision_note  TEXT,
  revision       INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
  created_at     TEXT NOT NULL,
  updated_at     TEXT NOT NULL,
  CHECK (scope = 'business' OR branch_id IS NOT NULL),
  CHECK ((entity_type IS NULL) = (entity_id IS NULL)),
  CHECK (status <> 'confirmed' OR (confirmed_by IS NOT NULL AND confirmed_at IS NOT NULL))
);
CREATE INDEX ix_business_memories_status ON business_memories(status, updated_at);
CREATE INDEX ix_business_memories_entity ON business_memories(entity_type, entity_id) WHERE entity_type IS NOT NULL;

-- A confirmed statement is not rewritten: a new one supersedes it.
CREATE TRIGGER trg_business_memories_confirmed_text BEFORE UPDATE OF statement, entity_type, entity_id, scope, branch_id ON business_memories
WHEN OLD.status <> 'candidate'
BEGIN SELECT RAISE(ABORT, 'a confirmed memory is not rewritten; add a new one that supersedes it'); END;

-- Superseded and rejected memories are history.
CREATE TRIGGER trg_business_memories_final BEFORE UPDATE OF status ON business_memories
WHEN OLD.status IN ('superseded','rejected')
BEGIN SELECT RAISE(ABORT, 'a superseded or rejected memory does not change'); END;

-- Nothing is deleted.
CREATE TRIGGER trg_business_memories_no_delete BEFORE DELETE ON business_memories
BEGIN SELECT RAISE(ABORT, 'memories are archived or rejected, never deleted'); END;

CREATE VIRTUAL TABLE memory_fts USING fts5(
  statement,
  tokenize = 'unicode61 remove_diacritics 2',
  prefix = '2 3 4'
);
