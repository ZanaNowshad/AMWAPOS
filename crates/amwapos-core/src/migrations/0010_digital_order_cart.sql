-- Pillar 6: a digital order is converted by loading it into a till cart; the
-- sale that commits that cart marks the order converted.
ALTER TABLE carts ADD COLUMN digital_order_id TEXT;
ALTER TABLE digital_orders ADD COLUMN cart_id TEXT;
