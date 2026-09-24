-- AI assistant: conversations, messages (provider-neutral content blocks),
-- and proposals. A proposal is a previewed change that a person confirms;
-- it is then executed by the normal audited command. Local to this computer.

CREATE TABLE ai_conversations (
  conversation_id TEXT PRIMARY KEY,
  user_id         TEXT NOT NULL,
  title           TEXT NOT NULL,
  untrusted_seen  INTEGER NOT NULL DEFAULT 0,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);
CREATE INDEX ix_ai_conversations_user ON ai_conversations(user_id, updated_at);

CREATE TABLE ai_messages (
  message_id      TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES ai_conversations(conversation_id),
  seq             INTEGER NOT NULL,
  role            TEXT NOT NULL CHECK (role IN ('user','assistant')),
  content_json    TEXT NOT NULL,
  input_tokens    INTEGER,
  output_tokens   INTEGER,
  stop_reason     TEXT,
  created_at      TEXT NOT NULL,
  UNIQUE (conversation_id, seq)
);

CREATE TABLE ai_proposals (
  proposal_id     TEXT PRIMARY KEY,
  proposal_number TEXT NOT NULL UNIQUE,
  conversation_id TEXT NOT NULL REFERENCES ai_conversations(conversation_id),
  kind            TEXT NOT NULL CHECK (kind IN ('price_change','stock_adjustment','purchase_order')),
  params_json     TEXT NOT NULL,
  preview_json    TEXT NOT NULL,
  risk            TEXT NOT NULL CHECK (risk IN ('low','medium','high')),
  risk_reasons    TEXT NOT NULL,
  status          TEXT NOT NULL CHECK (status IN ('proposed','executing','executed','rejected','failed','undone','expired')),
  result_json     TEXT,
  error           TEXT,
  created_by      TEXT NOT NULL,
  created_at      TEXT NOT NULL,
  decided_by      TEXT,
  decided_at      TEXT,
  undone_by       TEXT,
  undone_at       TEXT
);
CREATE INDEX ix_ai_proposals_status ON ai_proposals(status, created_at);
