-- Full-admin AI: a proposal may name any admin command ("command:<cmd>"),
-- in addition to the three original kinds. Nothing references ai_proposals,
-- so the table is rebuilt with the wider CHECK.
CREATE TABLE ai_proposals_new (
  proposal_id     TEXT PRIMARY KEY,
  proposal_number TEXT NOT NULL UNIQUE,
  conversation_id TEXT NOT NULL REFERENCES ai_conversations(conversation_id),
  kind            TEXT NOT NULL CHECK (kind IN ('price_change','stock_adjustment','purchase_order') OR kind LIKE 'command:%'),
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
INSERT INTO ai_proposals_new SELECT proposal_id, proposal_number, conversation_id, kind, params_json, preview_json, risk, risk_reasons,
  status, result_json, error, created_by, created_at, decided_by, decided_at, undone_by, undone_at FROM ai_proposals;
DROP TABLE ai_proposals;
ALTER TABLE ai_proposals_new RENAME TO ai_proposals;
CREATE INDEX ix_ai_proposals_status ON ai_proposals(status, created_at);
CREATE INDEX ix_ai_proposals_creator ON ai_proposals(created_by, created_at);
