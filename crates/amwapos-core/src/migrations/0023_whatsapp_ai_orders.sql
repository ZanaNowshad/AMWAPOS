-- WhatsApp AI orders: incoming messages on the existing WhatsApp link are
-- interpreted into structured, reviewable digital-order drafts. The draft is
-- a `digital_orders` row (status draft) like any other; confirming and
-- selling it stay in the normal order → till → sale workflow.

-- Idempotency marker: every inbox row is interpreted at most once.
CREATE TABLE wa_inbox_processing (
  inbox_seq    INTEGER PRIMARY KEY,
  status       TEXT NOT NULL CHECK (status IN ('processed','skipped','failed')),
  intent       TEXT,
  intent_band  TEXT,
  reason       TEXT,
  session_id   TEXT,
  source       TEXT NOT NULL DEFAULT 'rules',
  attempts     INTEGER NOT NULL DEFAULT 1,
  error        TEXT,
  processed_at TEXT NOT NULL
);

-- One ordering conversation per chat at a time.
CREATE TABLE wa_order_sessions (
  session_id          TEXT PRIMARY KEY,
  chat                TEXT NOT NULL,
  phone               TEXT,
  customer_id         TEXT REFERENCES customers(customer_id),
  customer_state      TEXT NOT NULL DEFAULT 'unknown' CHECK (customer_state IN ('matched','linked','ambiguous','provisional','unknown','staff')),
  customer_candidates TEXT,
  order_id            TEXT REFERENCES digital_orders(order_id),
  state               TEXT NOT NULL CHECK (state IN ('collecting','clarifying','ready','confirmed','cancelled','closed')),
  intent              TEXT,
  priority            TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('normal','high')),
  priority_reasons    TEXT,
  pending_json        TEXT,
  delivery_mode       TEXT NOT NULL DEFAULT 'unknown' CHECK (delivery_mode IN ('delivery','pickup','unknown')),
  address_raw         TEXT,
  address_json        TEXT,
  address_source      TEXT,
  zone_id             TEXT,
  delivery_fee_minor  INTEGER,
  fee_state           TEXT NOT NULL DEFAULT 'unresolved' CHECK (fee_state IN ('resolved','unresolved','not_applicable','staff')),
  ai_status           TEXT NOT NULL DEFAULT 'rules',
  staff_takeover      INTEGER NOT NULL DEFAULT 0,
  assigned_to         TEXT,
  handled             INTEGER NOT NULL DEFAULT 0,
  upsell_json         TEXT,
  last_seq            INTEGER,
  last_message_at     TEXT,
  revision            INTEGER NOT NULL DEFAULT 1,
  created_at          TEXT NOT NULL,
  updated_at          TEXT NOT NULL
);
CREATE UNIQUE INDEX ux_wa_order_sessions_active ON wa_order_sessions(chat) WHERE state IN ('collecting','clarifying','ready');
CREATE INDEX ix_wa_order_sessions_updated ON wa_order_sessions(updated_at);

-- Draft mutations and staff overrides, message by message.
CREATE TABLE wa_order_events (
  event_id   TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES wa_order_sessions(session_id),
  inbox_seq  INTEGER,
  kind       TEXT NOT NULL,
  source     TEXT NOT NULL CHECK (source IN ('rules','ai','person','system')),
  data_json  TEXT NOT NULL,
  user_id    TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX ix_wa_order_events_session ON wa_order_events(session_id, created_at);

-- Colloquial phrasing a person confirmed ("coke big" → a real product).
CREATE TABLE product_aliases (
  alias_norm TEXT PRIMARY KEY,
  product_id TEXT NOT NULL REFERENCES products(product_id),
  uses       INTEGER NOT NULL DEFAULT 0,
  created_by TEXT,
  created_at TEXT NOT NULL
);

ALTER TABLE digital_orders ADD COLUMN wa_session_id TEXT;
ALTER TABLE digital_orders ADD COLUMN delivery_fee_minor INTEGER;
ALTER TABLE digital_orders ADD COLUMN zone_id TEXT;
ALTER TABLE digital_orders ADD COLUMN area TEXT;
ALTER TABLE digital_orders ADD COLUMN flat TEXT;
ALTER TABLE digital_orders ADD COLUMN building TEXT;
ALTER TABLE digital_orders ADD COLUMN road TEXT;
ALTER TABLE digital_orders ADD COLUMN block TEXT;
ALTER TABLE digital_orders ADD COLUMN landmark TEXT;
ALTER TABLE digital_orders ADD COLUMN confirmed_by TEXT;

ALTER TABLE digital_order_lines ADD COLUMN requested_text TEXT;
ALTER TABLE digital_order_lines ADD COLUMN resolution TEXT;
ALTER TABLE digital_order_lines ADD COLUMN candidates_json TEXT;
ALTER TABLE digital_order_lines ADD COLUMN source_seq INTEGER;
ALTER TABLE digital_order_lines ADD COLUMN locked INTEGER NOT NULL DEFAULT 0;
ALTER TABLE digital_order_lines ADD COLUMN note TEXT;

ALTER TABLE payment_reviews ADD COLUMN order_id TEXT;
