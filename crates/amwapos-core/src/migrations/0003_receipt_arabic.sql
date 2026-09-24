-- Arabic product name captured at the time of sale, so receipts and reprints
-- show the name the customer saw even if the catalogue changes later.
ALTER TABLE sale_items ADD COLUMN product_name_ar_snapshot TEXT;
