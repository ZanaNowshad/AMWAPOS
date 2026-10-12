-- WhatsApp order autopilot and the order event feed
-- (docs/WHATSAPP_AUTOPILOT.md).
--
-- order_events: one row per order or delivery state change, in commit order.
-- Triggers write it, so every path that changes an order (staff screens, the
-- till, the autopilot, rows synced from other tills) produces the same feed.
-- A row is written only when the state actually changes (state_key differs
-- from the previous event of the same entity), so an upsert that rewrites an
-- unchanged row adds nothing. version counts per order (per delivery when the
-- delivery has no order), so a receiver can ignore an older version.
--
-- Consumers keep their own cursor: the customer notifier (order_event_cursors)
-- and each sync target (sync_sinks.cursor_seq). Events that one target could
-- not take after its retries are parked in sync_sink_failures and can be sent
-- again; the feed itself never changes.
--
-- All of these are hub-local (sync::LOCAL_TABLES): the hub receives every
-- till's orders and deliveries and is where WhatsApp and the targets run.
-- Nothing here is created for existing orders: the feed starts now.

CREATE TABLE order_events (
  seq          INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id     TEXT NOT NULL UNIQUE,
  source       TEXT NOT NULL CHECK (source IN ('order','delivery')),
  order_id     TEXT,
  delivery_id  TEXT,
  kind         TEXT NOT NULL,
  state_key    TEXT NOT NULL,
  version      INTEGER NOT NULL,
  created_at   TEXT NOT NULL
);
CREATE INDEX ix_order_events_order ON order_events(order_id, seq);
CREATE INDEX ix_order_events_delivery ON order_events(delivery_id, seq);

CREATE TRIGGER trg_order_events_order_insert AFTER INSERT ON digital_orders
BEGIN
  INSERT INTO order_events(event_id, source, order_id, delivery_id, kind, state_key, version, created_at)
  VALUES (lower(hex(randomblob(16))), 'order', NEW.order_id, NEW.delivery_id, 'order.' || NEW.status,
          NEW.status || '|' || NEW.payment_state,
          (SELECT COALESCE(MAX(version), 0) + 1 FROM order_events WHERE order_id = NEW.order_id),
          strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
END;

CREATE TRIGGER trg_order_events_order_update AFTER UPDATE OF status, payment_state ON digital_orders
WHEN COALESCE((SELECT state_key FROM order_events WHERE order_id = NEW.order_id AND source = 'order' ORDER BY seq DESC LIMIT 1), '')
     <> NEW.status || '|' || NEW.payment_state
BEGIN
  INSERT INTO order_events(event_id, source, order_id, delivery_id, kind, state_key, version, created_at)
  VALUES (lower(hex(randomblob(16))), 'order', NEW.order_id, NEW.delivery_id,
          CASE WHEN OLD.status <> NEW.status THEN 'order.' || NEW.status ELSE 'order.payment_' || NEW.payment_state END,
          NEW.status || '|' || NEW.payment_state,
          (SELECT COALESCE(MAX(version), 0) + 1 FROM order_events WHERE order_id = NEW.order_id),
          strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
END;

CREATE TRIGGER trg_order_events_delivery_insert AFTER INSERT ON delivery_orders
BEGIN
  INSERT INTO order_events(event_id, source, order_id, delivery_id, kind, state_key, version, created_at)
  VALUES (lower(hex(randomblob(16))), 'delivery', NEW.order_id, NEW.delivery_id,
          CASE WHEN COALESCE(NEW.outcome, '') NOT IN ('', 'delivered') THEN 'delivery.' || NEW.outcome ELSE 'delivery.' || NEW.status END,
          NEW.status || '|' || COALESCE(NEW.outcome, ''),
          (SELECT COALESCE(MAX(version), 0) + 1 FROM order_events
             WHERE (NEW.order_id IS NOT NULL AND order_id = NEW.order_id) OR (NEW.order_id IS NULL AND delivery_id = NEW.delivery_id)),
          strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
END;

CREATE TRIGGER trg_order_events_delivery_update AFTER UPDATE OF status, outcome ON delivery_orders
WHEN COALESCE((SELECT state_key FROM order_events WHERE delivery_id = NEW.delivery_id AND source = 'delivery' ORDER BY seq DESC LIMIT 1), '')
     <> NEW.status || '|' || COALESCE(NEW.outcome, '')
BEGIN
  INSERT INTO order_events(event_id, source, order_id, delivery_id, kind, state_key, version, created_at)
  VALUES (lower(hex(randomblob(16))), 'delivery', NEW.order_id, NEW.delivery_id,
          CASE WHEN COALESCE(NEW.outcome, '') NOT IN ('', 'delivered') THEN 'delivery.' || NEW.outcome ELSE 'delivery.' || NEW.status END,
          NEW.status || '|' || COALESCE(NEW.outcome, ''),
          (SELECT COALESCE(MAX(version), 0) + 1 FROM order_events
             WHERE (NEW.order_id IS NOT NULL AND order_id = NEW.order_id) OR (NEW.order_id IS NULL AND delivery_id = NEW.delivery_id)),
          strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
END;

-- Where order events go. kind: webhook (HTTP POST, HMAC-signed; ERPs,
-- spreadsheets through a script, n8n, Zapier) or csv (one line per event in
-- a file, e.g. on a shared folder a spreadsheet opens). A new target starts
-- at the current end of the feed; it does not receive history.
CREATE TABLE sync_sinks (
  sink_id      TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  kind         TEXT NOT NULL CHECK (kind IN ('webhook','csv')),
  config_json  TEXT NOT NULL,
  enabled      INTEGER NOT NULL DEFAULT 1,
  cursor_seq   INTEGER NOT NULL DEFAULT 0,
  attempts     INTEGER NOT NULL DEFAULT 0,
  next_at      TEXT,
  last_error   TEXT,
  last_ok_at   TEXT,
  delivered    INTEGER NOT NULL DEFAULT 0,
  created_by   TEXT NOT NULL,
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);

-- An event a target refused (permanently) or kept failing after its retries.
-- The target moves on; a person sends it again (retry) or dismisses it.
CREATE TABLE sync_sink_failures (
  sink_id     TEXT NOT NULL REFERENCES sync_sinks(sink_id),
  event_seq   INTEGER NOT NULL,
  attempts    INTEGER NOT NULL,
  error       TEXT NOT NULL,
  status      TEXT NOT NULL CHECK (status IN ('parked','retry','resolved','dismissed')),
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  PRIMARY KEY (sink_id, event_seq)
);

-- Built-in consumers of the feed (the WhatsApp customer notifier).
CREATE TABLE order_event_cursors (
  consumer    TEXT PRIMARY KEY,
  cursor_seq  INTEGER NOT NULL,
  updated_at  TEXT NOT NULL
);

-- One automatic decision per inbound message (a restart never answers twice).
CREATE TABLE wa_autopilot_log (
  inbox_seq   INTEGER PRIMARY KEY,
  chat        TEXT NOT NULL,
  action      TEXT NOT NULL,
  message_id  TEXT,
  created_at  TEXT NOT NULL
);
CREATE INDEX ix_wa_autopilot_log_chat ON wa_autopilot_log(chat, created_at);
