-- Stock reservations for digital orders (WhatsApp, phone, web, delivery).
-- A reservation is made at the business boundary "confirmed for
-- fulfilment" (never because a message was interpreted), for at most the
-- stock that is free (on hand minus other active reservations), and ends as
-- released (cancelled / expired) or converted (the order became a sale,
-- which moves the stock). Reservations never change stock_levels.
CREATE TABLE stock_reservations (
  reservation_id TEXT PRIMARY KEY,
  order_id       TEXT NOT NULL REFERENCES digital_orders(order_id),
  line_no        INTEGER NOT NULL,
  product_id     TEXT NOT NULL REFERENCES products(product_id),
  branch_id      TEXT NOT NULL,
  qty_milli      INTEGER NOT NULL CHECK (qty_milli > 0),
  wanted_milli   INTEGER NOT NULL CHECK (wanted_milli > 0),
  status         TEXT NOT NULL CHECK (status IN ('active','released','converted','expired')),
  expires_at     TEXT,
  created_by     TEXT,
  created_at     TEXT NOT NULL,
  closed_at      TEXT,
  closed_by      TEXT,
  close_reason   TEXT
);
CREATE UNIQUE INDEX ux_stock_reservations_line ON stock_reservations(order_id, line_no) WHERE status = 'active';
CREATE INDEX ix_stock_reservations_product ON stock_reservations(product_id, branch_id) WHERE status = 'active';
CREATE INDEX ix_stock_reservations_expiry ON stock_reservations(expires_at) WHERE status = 'active';
