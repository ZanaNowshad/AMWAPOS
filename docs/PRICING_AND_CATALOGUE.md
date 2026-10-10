# Pricing and catalogue (Merchant OS Wave 5)

The retail commercial foundation: what a barcode is, PLUs, scale barcodes,
likely duplicates and the product merge, Bahrain address details, the sales
channel, channel prices, pricing policies, rounding and margin protection.

Money is integer fils; quantities are thousandths; rates are basis points
(1% = 100 bp). The backend decides every price; the till sells offline; every
change is audited. Nothing in this wave changes a price, merges a product or
classifies a barcode by itself: a person decides.

Code: `barcodes.rs` (kinds, PLU, scale rules), `merge.rs` (duplicates and
merge), `catalog.rs` (`price_sql_for`, channel prices), `policies.rs`
(policies, rounding, review, apply), `pos.rs` (scan order, channel),
`address.rs` (governorate, directions, area), `reports.rs` (channel and price
change reports). Schema: `migrations/0031_wave5_commercial.sql`.
Tests: `tests/wave5.rs`, `tests/sync.rs` (Wave 5 convergence), `tests/perf.rs`,
`e2e/wave5.spec.ts`.

## Barcodes and their kind

A barcode's kind is one of EAN-13, EAN-8, UPC-A, UPC-E, Code 128, internal or
supplier code. It is stored only when a person chooses it. The digits only
suggest a kind ("looks like EAN-13" when the length and check digit fit). A
chosen kind must fit the code: an EAN-13 needs 13 digits and a valid check
digit. Barcodes from before Wave 5 stay "Not recorded".

## PLU

A PLU is a short number on the product itself, not on its barcodes:

- digits only, stored without leading zeros (`0042` is 42), at most 12 digits;
  Arabic-Indic digits are read as digits;
- unique among products;
- refused when it equals another product's barcode read as a number, and a
  barcode is refused when it reads as another product's PLU. The message names
  the other product, so the person can act on it.

The PLU is edited on the product (Barcodes tab) and sells through the normal
scan box: typed, scanned or read from a scale label.

## What a scan means

In this order; the first that answers wins:

1. a barcode stored on a product, exactly as scanned;
2. a PLU, leading zeros ignored;
3. a scale barcode rule;
4. otherwise the code is unknown and recorded for review, as before. Codes
   are never fuzzy-matched.

## Scale barcodes

A rule describes a label printed by a scale. Its fields are:

- name, prefix (1–6 digits) and total length (8–20);
- where the item code sits (start, length);
- whether the code holds a weight or a price;
- where the value sits (start, length) and its decimals (0–3);
- check digit (none or EAN), on/off, and priority.

Positions are 1-based, as in scale manuals.

- **Checked when saved.** Segments must fit inside the code, before the check
  digit when there is one. They may not overlap each other or the prefix, and
  the decimals cannot exceed the value's digits. An edit with a stale version
  is refused. Rules are switched off, never deleted.
- **Looked up, not scanned.** Only rules with the code's exact length and one
  of its first 1–6 digits as prefix are read (an index lookup). There is never
  a pass over every rule.
- **Fails closed.** The highest priority rule that fits reads the code. Two
  rules at that priority that both fit stop the sale with "More than one
  scale-barcode rule matches this code." Nothing is charged; raising one
  rule's priority settles it.
- **An exact barcode wins.** A product barcode equal to a scale code is used
  as a barcode.
- **Integer conversion.** The value in thousandths (weight) or fils (price) is
  the raw digits × 10^(3 − decimals). There is no floating point.
- **Weight labels** sell the weighed quantity at the product's price. The
  product must allow decimal quantities, otherwise the label is refused.
- **Price labels** sell one labelled pack at the printed price. The price is
  kept on the line: restoring a held sale does not reprice it, and the line
  never merges with another.
- **Sanity limits.** Settings → POS sets the largest weight (default 50 kg) and
  the largest price (default 100 BHD) a label may carry. A label above either
  limit, or at zero, is refused.
- **An item code no product owns** is refused ("no product has this PLU"),
  never guessed.
- **Evidence.** Rule id, value kind and value are kept on the cart line and
  the sale line.
- **Offline.** Rules are hub-owned and replicated to every till.

## Likely duplicates

Products are grouped by blocking keys:

- the same words and size;
- the same barcode number written differently (leading zeros);
- the same supplier item code;
- the same brand, word count and size.

Only products that share a key are compared, never every product with every
other. Blocks larger than 40 are skipped.

Names are normalized before comparing:

- lower case, Arabic digits read as digits, punctuation dropped, word order
  ignored;
- sizes made canonical: `330 ML` and `330ml` are the same, `1.5 L` is
  `1500ml`, `5 kg` is `5000g`, and Arabic unit words are understood;
- pack counts are kept: `6x330ml`.

A pair is suggested only with evidence:

- the same barcode number;
- or the same words and size, or words that differ by one letter (a typo, in
  words of five letters or more);
- or the same supplier item code and the same size.

Words that change what a product is are never noise. "Coca-Cola Zero 330ml"
is not a duplicate of "Coca-Cola 330ml", and neither is a different size,
"Full Fat" against "Low Fat", or "Tea 100" against "Tea 50".

Each pair shows its evidence (same barcode number, same name and size, same
supplier code, same category, similar price, same unit) and a match score.
A person decides:

- **Not duplicates** is remembered and the pair is not suggested again;
- **Review later** sets the pair aside;
- **Merge…** opens the merge preview.

## Product merge

The owner only (`catalog.merge`). A merge cannot be undone.

**Preview.** It shows every dependency: stock per branch, batches with their
dates, barcodes, saved names, supplier mappings, supplier terms, channel
prices that will not carry over, and the history that stays (sale lines,
refunds, movements, purchase lines, batches).

**Blocked.** These block the merge until they are finished or cancelled:

- open customer orders, purchase orders and requisitions;
- stock counts in progress;
- transfers not yet received;
- receiving drafts;
- supplier returns not yet sent;
- supplier invoices not yet posted;
- invoice scans being reviewed;
- sales in progress or on hold;
- active reservations;
- a WhatsApp catalogue listing.

Products sold differently (unit, weighed or not, stock tracked or not) cannot
be merged until they are made the same.

**Choices.** Explicit choices are needed when the selling prices differ, when
both products have a PLU, and per supplier when both have terms with that
supplier.

**What happens**, in one transaction:

- **Stock.** For each branch, each batch's remaining quantity leaves the
  retired product's batch and enters a new batch of the kept product. The new
  batch has provenance `merge`, points at the old batch, and keeps the
  supplier, dates, code and cost. Stock in no batch moves per location.
  Every move is a pair of `adjust` movements (`source_type` `product_merge`),
  so the total never changes. The merge checks this and refuses otherwise.
- **Cost.** The kept product's average cost becomes the quantity-weighted
  average of the two.
- **Links.** Barcodes, saved names, supplier mappings and resolved
  unknown-barcode links point at the kept product.
- **Supplier terms.** They move; where both products have terms with the same
  supplier, the chosen terms are kept.
- **Price and PLU.** The chosen price is applied through the price history.
  The chosen PLU moves.
- **Retired product.** It is archived, with "Merged into …" on its page.
- **Record.** A permanent merge record holds the preview, the choices and
  what moved, and the merge is audited.

**Idempotent.** It runs once per operation id. A retry returns the same
result. A merge whose preview has changed since it was shown is refused.

**History untouched.** Sales, sale items, receipts, refunds and movements are
never edited.

## Bahrain addresses

The existing model (Flat, Building, Road, Block, landmark) gains an optional
**governorate** (Capital, Muharraq, Northern, Southern) and **directions**.
They sit behind "More details" in the address form, on customers, drops and
digital orders. Neither is guessed from the block.

Areas are normalized: known areas take their usual spelling ("JUFAIR " →
"Juffair", "الجفير" → "Juffair"), and unknown names are kept as typed.

An address read from a WhatsApp conversation stays on that order. It never
overwrites the customer's saved address; "Save on customer" on a drop is a
person's explicit action.

## Sales channel

Every new sale records where it came from: `pos` (the till), `whatsapp`,
`phone`, `web` or `other`.

- **Set before pricing.** A till sale is `pos` from the first scan. A sale
  rung up from a customer order takes the order's channel before any line is
  priced, and that channel cannot be changed at the till. The cashier can
  mark a till sale as a phone order (or another channel); the catalogue lines
  are then repriced. Overrides, discounts and scale-label prices stay.
- **Kept through hold and restore.**
- **Fixed after completion.** It is stored on the sale, and sales are
  immutable.
- **Older sales** have no channel and show as **Not recorded**, never assigned
  one.
- **Separate from fulfilment.** Paid here or sent for delivery is a separate
  fact.

## Channel prices

There is one price resolver. `product_prices.price_type` gains price lists
`whatsapp`, `phone` and `web`; the till and `other` use retail. The fallback
is deterministic:

1. this branch's price for the channel (multi-branch only);
2. the channel's price;
3. this branch's retail price (multi-branch only);
4. the retail price.

Within a step the latest effective price wins, then the newest row. With
`retail` the SQL is exactly the earlier resolver: retail behaviour is
unchanged when there are no channel prices or policies.

**Where it is used.** The till, till search, customer-order estimates,
WhatsApp order totals, tickets and the WhatsApp catalogue all price through
it. An order's estimate therefore equals the sale it becomes.

**Setting prices.** Channel prices are set or removed on the product's Pricing
tab with `prices.manage`, on the hub only. Earlier rows are closed, never
edited, and each change is audited. A channel with no price of its own shows
**Using retail price**: on the product, on the till line and in the pricing
review.

**Snapshot.** The price list that priced each line is kept on the cart line
and the sale line, next to the price itself.

## Pricing policies

Policies only recommend: saving one changes no price.

**Scope.** All products, a category, a preferred supplier, a branch or a
sales channel.

**Terms:**

- either a **markup** on cost (net = cost × (1 + markup)) or a **target
  margin** on price (net = cost ÷ (1 − margin)). These are different: 25%
  markup is a 20% margin;
- an optional **minimum margin**;
- a **rounding step** (0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250,
  0.500 or 1.000);
- an optional **preferred ending** (for example x.950);
- a **priority**;
- an explicit **cost basis**: average cost, or the last cost paid.

**Precedence.** The most specific matching scope wins: channel, then branch,
then preferred supplier, then category, then all products. Within a scope the
higher priority wins. Two policies with the same scope and priority are shown
as an ambiguity and settled by name, then id, until a priority is changed.

**Floor.** The minimum margin that applies is the highest minimum among all
matching policies, so a broad rule's floor still protects a product with a
more specific policy.

### Rounding

- Margins are measured on the net price: the price without VAT, the shelf
  price ÷ (1 + VAT) when prices include it.
- The shelf price (what the customer pays) is rounded to the nearest step,
  halves up. With a preferred ending it goes to the nearest price that ends
  in it.
- It is then moved up, never down, until it meets the floor. The floor check
  is exact integer arithmetic: shelf × (10000 − min) ≥ cost × (10000 + VAT).
- A property test covers costs, VAT rates, every step, endings and minimums:
  rounding never breaches the minimum margin.

## Margin protection and the pricing review

The review (Catalog → Pricing review) lists active products with a cost in
five groups:

- **Below minimum margin**;
- **Cost changed** since the price was set;
- **No policy** applies;
- **Channel price missing**: a channel policy exists and the product has no
  price for that channel, so retail is used;
- **Recommendation available**.

Each row shows cost, current price and margin, the floor, the recommended
price and margin, and the policy.

**Actions.**

- **Inspect** opens the product.
- **Accept** applies the recommendation; **modify** applies a typed price.
- **Dismiss** hides the row while the cost and the suggestion stay the same.
- **Postpone** hides the row until a date.

A product below its minimum stays visible even when dismissed or postponed.

**Bulk apply.** A preview lists old and new prices, margins and the prices
below a floor. Applying is then:

- all or nothing, and idempotent per operation id;
- audited, with a batch record;
- recorded in the price history only for prices that actually change, with
  the policy and batch on each new price row.

A price below a minimum margin needs `pricing.policy`, or an approval bound to
exactly those prices.

**Performance.** One pass over the catalogue with policies in memory: about
0.9 s for 100,500 products in the release benchmark.

## Reports and dashboard

- **Sales by channel:** transactions, items, sales, average basket, refunds
  (against the sale's channel, on the day refunded), net sales, and gross
  margin with financial reports. Gross margin is sales minus cost of goods. It
  is not a channel profit: delivery, fees and commissions are not included.
  Sales without a channel show as Not recorded.
- **Price changes:** every applied change with its list, old and new price,
  reason, policy and who made it.
- **Dashboard cards** load separately from the dashboard and link to their
  workflow: prices below minimum margin, price recommendations, costs changed,
  likely duplicates.

## Permissions

| Permission | Who has it by default | What it allows |
| --- | --- | --- |
| `catalog.merge` | Owner only (excluded from Manager) | Merge products |
| `pricing.policy` | Owner, Manager | Set policies; apply prices below a minimum margin |
| `barcode_rules.manage` | Owner, Manager | Set up scale barcode rules |
| `prices.manage` (existing) | Owner, Manager | Channel prices, pricing review, apply |
| `products.manage` (existing) | Owner, Manager | PLU, barcode kind, duplicate decisions, merge preview |

Cashiers get none of these. The new permissions are added to existing
built-in roles once on upgrade; a permission the owner removed is never added
back.

## Sync ownership

| Table | Ownership | Why |
| --- | --- | --- |
| `scale_barcode_rules` | Hub, replicated | Tills read scale labels offline |
| `products.plu`, `product_barcodes.kind`, channel rows in `product_prices` | Hub, replicated (existing tables) | Offline pricing and scanning |
| `sales.channel`, sale/cart line evidence | Append (sales) / per till (carts) | Facts of the sale |
| `pricing_policies` | Back office (hub only) | Recommendations only; the prices they lead to replicate as prices |
| `price_recommendation_decisions`, `price_change_batches` | Back office | Review state and batch records |
| `product_duplicate_decisions`, `product_merges` | Back office | Catalogue maintenance records |

`tests/sync.rs` covers the whole path. A rule and a PLU are created on the hub
and the till receives them, while policies stay off the till. The till refuses
to edit the rule. It sells a scale label offline. The hub sets a WhatsApp
price and a policy. After syncing, the sale arrives with its evidence and
channel, the till prices WhatsApp sales like the hub, and stock converges.

## AI

The AI can read:

- likely duplicates, the merge preview and past merges;
- scale rules, and a test of what a code means;
- channel prices;
- pricing policies, the pricing review and an apply preview;
- the commercial summary.

It cannot:

- merge, set scale rules, PLUs or barcode kinds;
- set the sale channel, channel prices or policies;
- apply prices, or dismiss or postpone recommendations. None of these has an
  AI tool.

Existing AI tools cannot set a PLU, a governorate or directions, or an order's
channel: the proposal is refused. An AI price proposal below a policy's
minimum margin is refused when proposed and checked again when executed. Only
a person can price below a minimum margin. Customer address privacy rules are
unchanged.

## Migration (0031)

The migration fabricates nothing:

- no barcode kind, PLU, scale rule, policy, channel price, duplicate decision,
  merge, governorate or channel is created;
- existing sales keep a NULL channel ("Not recorded");
- existing prices are unchanged.

`stock_lots` is rebuilt once to allow provenance `merge`. The copy is
verified (row count and quantity sum) inside the migration, and the
immutability triggers are recreated. `tests/wave5.rs`
`upgrading_fabricates_nothing` upgrades a schema-30 database and checks all of
this.

## Invariants (checked in `tests/wave5.rs`)

1. A merge never changes total stock.
2. A merge never changes issued receipts.
3. Historical sale money snapshots never change.
4. One barcode or PLU never answers to two active products without an
   explicit ambiguity (conflicts refused at save).
5. Ambiguous scale rules never charge a customer.
6. The same cart, channel and synchronized configuration give the same price.
7. A pricing policy never silently rewrites a current price.
8. Rounding never breaches the minimum margin.
9. Retail behaviour is unchanged without channel prices or policies.
10. The till prices and sells offline, independent of hub, internet and AI.

## Performance (100k products, release build, this environment)

| Measure | Wave 5 | Target / Wave 4 |
| --- | --- | --- |
| P95 barcode scan | 1.57 ms | 50 ms / 1.23 ms |
| P95 scale-label scan (200 rules) | 1.45 ms | 50 ms |
| P95 PLU scan | 1.39 ms | 50 ms |
| P95 product search | 5.16 ms | 150 ms / 4.79 ms |
| P95 sale commit | 11.64 ms | 500 ms / 9.73 ms |
| Duplicate review, 100,500 products | 0.99 s | — |
| Pricing review, 100,500 products | 0.91 s | — |
| Dashboard cards (both reviews) | 1.7 s, loaded apart from the dashboard | — |

The sale path gained only a cart channel read and the line evidence columns.
Benchmark figures move by about ±2 ms between runs here.

## Not proven here (external)

- Real scale printers and their label formats (CAS, DIGI, Mettler and others),
  including EAN check digits printed by the scale.
- Real catalogues with real duplicates, and real merges in a store with
  history.
- Pricing policies against real supplier costs and VAT setups.
- Arabic text on real till screens and receipts.

## Wave 6: promotions are not prices

Promotions, coupons and bundles are in [PROMOTIONS_AND_BUNDLES.md](PROMOTIONS_AND_BUNDLES.md).

Relationship with this document:
- An offer changes what a sale pays while it runs. It never writes a price, never changes a
  channel price and never moves a pricing-policy floor.
- The resolver above is step 1 of the pipeline; offers start from its result.
- Policies and margin floors are not applied to offers. The offer editor shows each covered
  product's offer price and margin and warns about below-cost and negative-margin sales; nothing
  is rewritten automatically.
- Price-embedded scale labels never take ordinary offers. Weight labels do.
- A bundle's price comes from the resolver like any product's.

## Wave 8: evidence, not prices

- Price lists and quotations can be kept in the Document Library and linked to a supplier or
  product. A document never changes a price; prices change only through the pricing commands.
- Business Memory facts about a product show "May be outdated" once the product record changes
  after they were confirmed. The assistant ranks the product record above memory.
