# Promotions, coupons and bundles (Wave 6)

This document covers the promotional part of commerce: offers, coupon codes and virtual bundles
(kits and hampers). There is one pricing pipeline and it lives in the backend. The till, the
WhatsApp order desk, web orders and reports never compute an offer themselves; React only
displays what the backend returns.

Code:
- `crates/amwapos-core/src/promotions.rs`: the engine;
- `promo_admin.rs`: administration, coupons, the dashboard list;
- `bundles.rs`: bundles;
- `pricing.rs`: the money arithmetic, extended;
- `pos.rs`, `sales.rs`, `refunds.rs`, `receipt.rs`: integration.

Schema: migration `0032_wave6_promotions.sql`.
Tests: `tests/wave6.rs` (22), `tests/sync.rs` (Wave 6 test), engine and pricing property tests,
`tests/perf.rs` (promotion stress), `e2e/wave6.spec.ts`.

## 1. The pipeline (order is fixed, documented and tested)

```
RESOLVED PRICE              Wave 5 resolver: branch + channel price list (retail fallback)
→ ITEM OFFERS               percent, amount, fixed price, quantity deal, Buy-X-Get-Y; at most one per line
→ BASKET OFFER              one "spend and save" offer, on what the item offers left
→ COUPON OFFER              the sale's one coupon unlocks one offer
→ MANUAL LINE DISCOUNT      (a line with one never takes automatic offers: no double discount)
→ MANUAL CART DISCOUNT      applies to the amount after offers
→ LOYALTY                   unchanged
→ VAT PER LINE              per line, or per component for a bundle
→ SALE SNAPSHOT             frozen at commit
```

`pricing::price_cart` takes the offer amount of each line (`promo_discount_minor`) from
`promotions::run`. With no offers it is exactly the Wave 5 arithmetic. The tests
`without_promotions_pricing_is_exactly_wave5` and the receipt and loyalty regressions in the full
suite prove this.

The whole calculation runs again on every read of the cart (scan, quantity, channel, customer,
coupon, hold/restore). Nothing incremental is stored. It runs once more inside the commit
transaction, so the sale records what the engine says at that moment.

**Order estimates.** Estimates for WhatsApp, phone and web orders show the normal (channel)
price. Offers and coupons apply when the order is rung up at the till, through this same
pipeline. The order screen says so; it is never a second calculator.

## 2. Promotions

### Model

Table `promotions`:
- identity and text: id, name, Arabic name, description;
- status: `draft | active | paused | ended | archived`;
- kind: `percent | amount | fixed_price | quantity | bxgy | basket`;
- target: `items` (products and categories in `promotion_targets`, roles `buy`/`get`) or `all`;
- schedule: start/end in store-local `YYYY-MM-DDTHH:MM` (start inclusive, end exclusive);
- scope: branch list, channel list (NULL = all);
- conflict settings: priority, stackable, requires_coupon;
- benefit: percent (basis points), amount, price, X/Y, max uses, threshold;
- record keeping: created by, timestamps, version.

There is no stored "currently active" flag. Whether an offer applies is derived from status +
schedule + scope at the sale's time. The editor shows the derived state: Running, Scheduled,
Past its end date, Paused, Draft, Ended, Archived.

There is no scripting language: every rule is one of the six kinds above.

### Lifecycle

`draft → active ⇄ paused → ended → archived` (draft and paused can also be archived).
- Reaching the start date does **not** switch an offer on. A person with `promotions.manage`
  switches it on, and the dashboard warns when a draft is about to start.
- Nothing is ever deleted: a database trigger refuses deletes, because a used offer is sale
  evidence.
- Ended and archived offers cannot be edited. Duplicate the offer instead.

### Schedule semantics

- **Time used:** the sale's (cart's) time in the store's timezone.
- **Midnight:** a sale at 23:59 belongs to the day it is rung; an offer ending at
  `2026-10-08T00:00` stops at midnight exactly.
- **Trading-day cutoff (Wave 2):** schedules use the local wall clock, not the business date. A
  sale at 00:30 that is counted in the previous trading day still gets only the offers running
  at 00:30.
- **Offline till:** the till uses its own clock and the configuration it last synced. A schedule
  edit on the hub reaches the till at the next sync; a sale committed before then keeps what it
  was priced with.
- **History:** schedule and benefit edits never rewrite a committed sale. What each sale took is
  frozen in `sale_item_promotions` (immutable by trigger).

### Kinds and safety

| Kind | Example | Rule |
| --- | --- | --- |
| percent | 10% off | `percent_of(line, bp)`, validated 0.01–100% |
| amount | 0.200 off each | per unit (per kg for weighed lines); capped at the line value |
| fixed_price | now 0.400 | never above the normal price (no increase) |
| quantity | 3 for 1.000 | groups of N whole units, dearest units first; leftover units at full price |
| bxgy | buy 2 get 1 free / at 50% | rewards are the cheapest reward units, qualifiers the dearest; X and Y may be the same product; max uses per sale |
| basket | spend 10.000 save 10% (or 1.000) | measured on the eligible lines after item offers, before its own discount (never self-referential) |

Every amount is integer fils and every rate is basis points. No offer takes more than a line is
worth, so a line never goes below zero and nothing becomes cash. Splits use the canonical
allocator `money::allocate` (largest remainder), so parts always sum exactly.

**Bounded work.** Quantity and Buy-X-Get-Y offers look at whole units. At most 10,000 units per
offer per sale are considered (`MAX_OFFER_UNITS`); units beyond that are charged at the normal
price. Buy-X-Get-Y matching is linear in the number of units.

### Eligibility

Product, category, branch, channel, schedule, minimum quantity (quantity and BXGY), minimum
spend (basket) and coupon requirement. There is no loyalty tier or customer segmentation.

These lines never take an automatic offer:
- custom items;
- lines whose price was overridden;
- lines with a manual discount;
- **price-embedded scale labels** (the printed price is the price).

Weight labels take offers like any other line.

### Conflicts and stacking

- **Item offers:** ranked by priority (higher first), then by the benefit each would give on its
  own (larger first), then by promotion id. Each line takes at most one item offer.
- **Basket offers:** at most one applies.
- **Coupons:** one per sale.
- **Stacking:** a later layer reaches a line that already has an offer only when every offer
  involved is stackable. The conservative default is "not stackable".
- **Complexity:** each candidate offer is evaluated a bounded number of times over the cart's
  lines. Candidates come from an index on the cart's products and categories plus the
  whole-basket offers, never every product against every offer.

**Determinism:** the same configuration, cart, channel, time and coupon give the same result. The
lines are put in a canonical order (product, unit price, quantity, line id) before any grouping or
split. A property test checks 3,000 random baskets in reversed and rotated order. Tests also
check that order does not matter for quantity deals and Buy-X-Get-Y.

### Explanations

The cart lists why each relevant offer did not apply, using the engine's own messages:
- "Not active yet (starts …)."
- "Only for WhatsApp sales."
- "Requires 3 items."
- "Requires a basket of at least …"
- "Another higher-priority offer was used."
- "Coupon is required."
- "This item came from a fixed-price scale label."
- "This item already has a manual discount."

Drafts and paused offers are not shown to the cashier.

### Manual discounts

Existing permissions and approvals are unchanged.
- A manual line discount takes the line out of automatic offers; removing it brings the offer
  back.
- A cart discount applies to what is left after offers. A cart discount larger than that is
  refused.

### Margin

Promotions are not the Wave 5 margin floors and are never rewritten automatically. The editor
shows the offer price and margin of each covered product (with cost permission) and warns about
negative margin and below-cost sales. The dashboard lists running offers that sell below cost.

## 3. Coupons

A coupon is only a key: it unlocks an offer marked "Needs a coupon code", and the promotion
engine computes the money.

Codes are normalized deterministically: spaces removed, upper case, Arabic digits read as digits,
hyphens kept. "save 10" equals "SAVE10"; "SAVE-10" is a different code.

There are two kinds:
- **reusable**: works on every till, even offline;
- **limited**: at most N uses in total. Only the main computer (hub or standalone store) holds
  every redemption, so only it accepts a limited code. A till says "This coupon needs the main
  computer to verify it." and the sale goes on at the normal price. A coupon never blocks
  checkout.

Redemption:
- **A redemption is written only at commit.** It is never written for a preview, a held cart or a
  cancelled sale.
- The record keeps the coupon, offer, sale, branch, device, user, customer, time, amount and the
  sale's operation id. It is append-only and unique per coupon and sale.
- A replayed commit (same operation id) returns the first result and writes nothing new.
- At commit the limited count is checked again under SQLite's single writer, so two sales cannot
  both take the last use. The second sale sees "This coupon has already been used." A sale whose
  previewed total included the coupon is refused with "The sale total changed", and the cashier
  re-takes payment at the right total.
- A held cart keeps its coupon and checks it again when restored. The restore notice says why if
  it no longer applies.

The cart shows one of these states:
- Applied
- Invalid
- Expired
- Not active yet
- Already used
- Needs main computer verification
- Not eligible for this basket
- Not for this channel / branch
- Switched off

**No override exists.** There is no way to accept an expired, used or limited coupon without the
main computer. Adding one later would need an explicit, permissioned, bound and audited approval.

## 4. Bundles, kits and hampers (virtual)

A bundle is a product (the parent) with a versioned list of components and quantities:
- tables `bundles` and `bundle_components`;
- the components of a version are immutable;
- an edit saves version + 1.

It is assembled when sold. Nothing is manufactured in advance and the parent has no stock: saving
a bundle switches its stock tracking off, and a parent with recorded stock is refused.

| Topic | Rule |
| --- | --- |
| Availability | min over stock-tracked components of floor(stock / quantity per bundle), with the limiting component; one query |
| Price | the parent's price from the Wave 5 resolver |
| Offers | the bundle line can take promotions and coupons; components never take product offers |
| Money split | the line's net (after offers and discounts) is allocated over the components by their normal value (component retail price × quantity) with the canonical allocator; sums exactly |
| VAT | per component at its own rate (standard, zero and exempt in one hamper) |
| Sale lines | one `sale_items` row per component, with the bundle name, version, line and quantities; receipts print the bundle as one line with its items beneath |
| Stock | each component leaves stock in the same commit; the parent never moves; the negative-stock rule applies to components |
| Cost (COGS) | the sum of the components' average costs |
| Refunds | whole bundles only, every component for the same number of bundles, from the frozen split; stock returns per component |
| Holds | the line keeps the version it was priced with; restore applies the current version with a notice ("contents changed"), or removes a switched-off bundle |
| Safety | no weighed components, no fractional quantities, no bundles inside bundles, a parent cannot be weighed or have a PLU |
| Replenishment | works on the components, which carry the stock |

## 5. Sale snapshot, receipts, refunds

At commit the sale freezes:
- the normal price and price source (Wave 5);
- `promo_discount_minor` per line;
- `sale_item_promotions` (offer id and name, layer, coupon code, amount per sale line);
- the coupon redemption;
- the bundle identity and version, the per-component allocation, VAT and totals.

The issued receipt snapshot (Wave 1) is written in the same transaction.

Receipts show savings by name with no internal ids, for example:
- "Weekend offer -0.500" under the line;
- "Coupon SAVE10 -0.250" in the totals;
- "Family Hamper 1 x 7.500" with its items;
- "You saved".

Labels are bilingual when the receipt is set to bilingual.

Refunds use the frozen amounts (proration of the stored line totals and VAT), never today's
offers.

## 6. Offline and sync

| Table | Class | Why |
| --- | --- | --- |
| promotions, promotion_targets, coupons, bundles, bundle_components | Hub-replicated | definitions; tills cannot edit them |
| sale_item_promotions, coupon_redemptions | Append-only | sale evidence, made where the sale is made |
| carts.coupon_code, cart_lines.bundle_version | Local | the sale in progress |

Tested (`sync.rs`, Wave 6 test):
1. The hub pauses an offer while a till is offline. The till keeps selling with the configuration
   it had.
2. A lost push response is retried. There is one redemption and one set of offer rows.
3. Stock of the bundle components converges, and the bundle parent never moves.
4. After sync the pause applies to new carts, and the committed sale keeps its offer.

The commit never depends on the hub.

## 7. Permissions, audit, idempotency

- `promotions.manage`, `coupons.manage`, `bundles.manage`: the owner always has them, managers
  have them by default, cashiers do not. They are added to existing built-in roles once on
  upgrade.
- Cashiers sell and apply coupons.
- Every administrative change is audited with before/after: offer created/changed/status, coupon
  created/changed, bundle created/changed. The cart's coupon is audited too.
- Save, status and coupon commands take an operation id. A replay returns the first result, and
  the same id with different content is refused. A stale version is refused with "changed by
  someone else".

## 8. Reports and dashboard

Reports (no causal claims, no "loss", no "net profit"):
- **Promotions:** sales used in, discount given, revenue (ex VAT), cost and gross margin of the
  lines each offer touched.
- **Coupon redemptions:** per code: redemptions, discount, uses left.
- **Bundles:** units, revenue, cost of items, gross margin, items taken from stock.

Dashboard items (actionable only; each opens its screen):
- offer selling below cost;
- offer starting soon that covers no products;
- draft about to start;
- offer ending soon;
- limited coupon used up;
- bundle that stock cannot make.

## 9. AI

The assistant may read:
- offers, their state and performance;
- the explanation of one offer (coverage, margins, conflicts, coupons);
- the attention list;
- bundles and their availability.

It may check an unsaved draft offer (`promotions.preview`) to propose one. It may **not**:
- save or switch on an offer;
- create or change coupons;
- change stacking or bundles;
- apply a discount at the till;
- change any sale.

These commands are listed as forbidden in `ai_tools::NO_TOOL`. The hub test fails if any command
is neither a tool nor listed with a reason.

## 10. Migration

Migration 0032 only creates empty tables and nullable columns. No offer, coupon, redemption or
bundle is invented. Old manual discounts stay manual discounts, and old sales, receipts and
prices are untouched. Old sales have `promo_discount_minor` NULL ("not recorded").

## 11. Known limits

- Quantity and Buy-X-Get-Y offers count whole units only (weighed lines take percent, amount
  and fixed-price offers).
- Partial-bundle refunds are not offered; whole bundles are refunded exactly.
- The money split of a bundle uses the components' current retail prices as weights at the time
  of pricing. The split is frozen with the sale.
- Order estimates (WhatsApp, phone, web) show normal prices; offers apply at the till.
