# Inventory truth: batches, expiry, waste, days of stock left (Wave 3)

Plan: [MERCHANT_OS_PLAN.md](MERCHANT_OS_PLAN.md).

## One stock truth

Stock is `stock_movements`, and only that. `stock_levels` is the cached
sum, as before.

Nothing in Wave 3 keeps a second quantity:
- A batch (`stock_lots`) records facts only: product, branch, location,
  supplier, purchase order, receipt, the supplier's batch code, dates, the
  quantity received and its unit cost. It holds no "remaining" quantity
  that anyone could edit.
- What a batch has left is worked out from the movements each time it is
  read (`lots::replay`).

Batches plus the stock in no batch always equal the stock on hand. Every
Wave 3 test asserts this after every step.

The movement table was not rebuilt. Wave 1 already added the `waste` type
and the `lot_id` column. A movement records the batch only when the batch
is known.

## How a batch's balance is worked out

The product's movements in one branch are replayed in their own time order:
`created_at`, then `movement_id`. Ids are generated in strictly increasing
order within a process, so movements written in the same millisecond keep
their order.

| Movement | Effect |
| --- | --- |
| Tagged with a batch, into it (receiving, count into a batch, a waste reversal) | The batch grows. If more was sold earlier than there was stock, that shortfall is settled from the new batch first. |
| Tagged with a batch, out of it (waste, count) | Evidence: the batch shrinks by exactly that amount. |
| Not tagged, into stock (a refund, an adjustment, receiving without a batch) | Stock in no batch grows. |
| Not tagged, out of stock (a till sale, an adjustment) | Uses stock in no batch first (the older stock from before batches were kept), then the batches **first-expiring-first-out**. |

First-expiring-first-out order is:
1. the earliest expiry;
2. undated batches after dated ones;
3. then the earliest received;
4. then the batch id.

**Estimated, not observed.** The amount taken from batches by untagged
sales is an *estimate*. A till sells without knowing which batch the
customer picked. The estimate is computed when read and is never stored,
so it can never be mistaken for evidence later. Screens label it "Estimated
sold (first expiring first out — the till does not know the batch)".

**Evidence wins.** Suppose someone records waste of 8 from batch A, but the
estimate had already taken A down to 5. Batch A still physically had them.
So the estimate moves 3 of the earlier sales to the next batch (or to the
stock in no batch), and A then loses the 8. The total never changes.

**Same answer in any order.** Only the movements' own times matter. The
same movements, received in a different order, give the same batches. This
is tested by writing the same movements in opposite order into two
databases, and by a till that sells offline while the hub records waste.

## Offline tills

Tills never know or need batches:
- a sale does no batch work;
- `stock_lots`, `lot_corrections` and `waste_records` stay on the hub;
- a till refuses to record waste or batches.

The hub's batch-tagged movements still reach the tills, so their stock is
right. When a till's offline sales arrive, the hub's replay places them at
their own times. Nothing is stored, so nothing has to converge; the answer
is simply recomputed. Checkout does no extra work: the sale-commit P95 is in
[STATUS.md](STATUS.md).

## Receiving with a batch

Receiving accepts a batch code, an expiry date, a production date and the
date's kind on each line. Any of these makes a new batch, written in the
same transaction as the receiving movement, and the receipt's operation id
makes a retry safe. This works for:
- direct receiving;
- purchase-order receiving;
- receiving drafts from supplier documents.

**Products.** Product → Inventory → Batches and expiry sets two things:
- "Ask for batch and expiry when receiving" (`track_lots`);
- what the date on the pack means: expiry (use by) or best before.

Products without lots work exactly as before.

**Dates.**
- An impossible date (not a real date, or outside 2000–2100) is refused.
- A suspicious date asks the person to confirm before it is kept:
  - expiry before production;
  - expiry before the day of receipt ("arrived expired");
  - expiry more than N years away (setting);
  - production after receipt.

No shelf-life is assumed for any product.

**Dates read from supplier documents.** The supplier document reader
suggests an expiry when a line says "EXP 31/12/2026", "BB 06/2027" or "Exp:
2027-03-15". The suggestion is stored on the draft line as
`expiry_source = document`, unconfirmed, and posting the draft is refused
until a person confirms the date, changes it or clears it. A batch made
from a confirmed document date keeps `expiry_source = document`.

## Old stock

The upgrade (migration 0029) creates:
- no batches;
- no expiry dates;
- no waste;
- no changed movements.

Existing stock shows as **Not in a batch**. Sales use it first, so it
depletes naturally. A person can also **count stock into a batch** after
reading its date off the pack. That writes two adjustment movements (out of
no batch, into the new batch), and stock on hand does not change. The
upgrade test builds a schema-28 store and checks that stock, movements and
the batch count are unchanged.

## Correcting a batch

A batch's facts are permanent, enforced by a trigger. A correction (code,
dates or the date's kind, with a reason) is a `lot_corrections` row. The
newest correction applies, the original stays in the history, and the
correction is audited.

## Expiry

**State.** Each batch has one current state:
- depleted;
- no date;
- **expired** (use-by date passed);
- **past best-before**;
- urgent (default 7 days or less);
- soon (30 days or less);
- later (90 days or less);
- healthy.

The thresholds are in Settings → Inventory. The state is computed from the
date and the balance, never stored, so it cannot drift.

**Expiry is a condition, not an action.** An expired batch stays in stock
until a person records waste, adjusts it, or acts some other way. The test
`expired_stock_stays_until_someone_records_what_happened` checks this.

**The Expiry page** (Inventory → Expiry) has four groups: Expired, Urgent,
Soon and Later. For each batch it shows:
- the batch;
- "Expires in 6 days" or "Past best-before by 2 days";
- how much is left;
- where it is;
- its value (for people who can see costs);
- the recent daily sales.

It also shows how much is **likely left at expiry**. This is worked out
with recent demand used up in order: older stock first, then earlier
batches, then this one.

The page keeps two kinds of figure apart:
- **At risk:** expired stock still on hand, stock expiring within 7, 14,
  30, 60 and 90 days, and the value likely left.
- **Already lost:** expired waste recorded this month.

**Markdowns.** The page shows price-reduction scenarios (10, 20, 30 and
50 % off, with the resulting margin and "below cost") as options only.
Nothing changes automatically. The page links to the product, where the
price is changed through the normal workflow. Promotions are Wave 5.

## Waste

Inventory → Waste, or from a batch.

**Reasons:**
- expired;
- damaged;
- spoiled;
- broken;
- **shrinkage / unexplained difference** (a neutral term: never "theft"
  without evidence);
- used in the shop;
- rejected at receiving;
- other.

**Records.** Each waste record keeps:
- the product, and the batch if known;
- the quantity;
- the unit cost: the batch's cost when the batch is known, else the average
  cost at the time;
- the total cost;
- the reason, a note and the business date;
- who recorded it, and the approver;
- the movement and the operation id.

**Stock.** Recording waste writes exactly one movement (type `waste`), and a
retry with the same operation id writes nothing more. Stock is never edited
directly.

**Approval.** A manager approves (bound to that exact request, as in Wave 1)
when:
- the value at cost is over Settings → Inventory → "Waste that needs a
  manager" (default BHD 20.000);
- or the reason is shrinkage (setting, on by default).

Small waste needs no extra step.

**Reversal.** A compensating movement (type `waste`, source
`waste_reversal`) brings the stock back. The record stays, marked reversed.
Only the reversal may be added, once (trigger), and reversed waste is not
counted in the summaries.

## Waste and expiry figures

Waste summary (Inventory → Waste → Summary, needs cost visibility):
- waste at cost and quantity;
- broken down by reason, product, category, supplier (when the batch's
  supplier is known) and day.

The two ratios are defined on the screen:
- **% of sales** = waste at cost ÷ net sales excluding VAT (sales less
  refunds and voids, by business date) for the same days;
- **% of goods received** = waste at cost ÷ goods received at cost for the
  same days.

## Days of stock left

Inventory → Days of stock left. For each active product that tracks stock:

- **available** = on hand − active order holds;
- **daily demand** = net units sold over the last 7, 30, 60 or 90 completed
  days (yesterday back) ÷ the days used. Net units sold is units sold less
  units refunded or voided, each on its own business date, the same
  definition as the sales reports (`lots::net_units_sold`), branch by
  branch. For a product younger than the window, only its own days are
  used;
- **days of stock left** = available ÷ daily demand, to one decimal, with
  the stock-out date;
- **stock on order** (ordered, not yet received) is shown as its own
  figure, "with stock on order", and is never mixed into the main one.

When it cannot be said, the answer gives the reason instead:
- no stock;
- no sales recently;
- not enough recent sales to estimate (under 7 days of history).

It never shows infinity. Dead stock (the existing report) is a separate
idea.

Wave 4 (procurement) uses the same demand functions for Suggested orders
([PROCUREMENT.md](PROCUREMENT.md)): `lots::demand` for one product and
`lots::demand_all`, which computes every product in a fixed number of
grouped queries. Suggested orders also leave out expired use-by stock and
use the batches to warn about stock expiring before the next delivery.

Goods refused at receiving never enter stock and are not waste; a supplier
return is a `supplier_return` movement out of the batch named on the line
(evidence, like waste), reversed by a compensating movement.

## Permissions

| Permission | Who by default | Allows |
| --- | --- | --- |
| `inventory.view` (existing) | | Batches, expiry, waste list, days of stock left |
| `products.view_cost` (existing) | | Values and the waste summary |
| `lots.manage` | manager, inventory | Correct batch details; count stock into a batch |
| `waste.record` | manager, inventory | Record waste |
| `waste.approve` | manager | Approve large waste and shrinkage; reverse waste |

Owners have all of them. Cashiers have none. Upgrades add these to
built-in roles once and never re-add a permission the owner removed.

The AI can read batches, expiry, waste and days of stock left. It cannot
record or approve waste, change batches or dates, change stock or change
prices. Those commands are in `NO_TOOL` and are classed as financial
commits.

## Ownership

| Table | Where |
| --- | --- |
| `stock_movements` (incl. `lot_id`) | Append-only, synced as before |
| `stock_lots`, `lot_corrections`, `waste_records` | Hub only (`sync::LOCAL_TABLES`); a till refuses |
| `products.track_lots`, `expiry_kind` | With the product (hub-owned) |
| Receiving-draft batch fields | With the draft (hub only) |

## Tests

- `tests/wave3.rs` (12):
  - receiving makes one batch and keeps old stock outside batches;
  - older stock first, then first expiring;
  - tie-breaks, and selling beyond the batches;
  - evidence beats the estimate;
  - waste exactly once, and reversal;
  - expired stays until recorded;
  - approval and permissions;
  - counting into a batch, and corrections;
  - document dates need a person;
  - days of stock left, and why;
  - the upgrade from schema 28;
  - arrival-order independence.
- `tests/sync.rs`: batches converge after an offline till sells while the
  hub sells and records waste, including a lost reply and a retry.
- `lots.rs` unit tests: expiry suggestions, and cover states.
- `e2e/wave3.spec.ts`: receive with batch and expiry → Expiry → record
  waste → Waste → Days of stock left.

## Not proven here (external)

- How staff identify batches on real shelves and packaging.
- Real supplier labels and documents (the date patterns).
- The real disposal routine.
- Use on the 1024×768 till panel.
- A till returning from offline over the store Wi-Fi.

## Product merge and stock (Wave 5)

A product merge moves stock with paired `adjust` movements (`source_type` `product_merge`) per
branch and location, so the total never changes. Each batch's remaining quantity leaves the
retired product's batch and enters a new batch of the kept product. The new batch has provenance
`merge`, points at the old one through `merged_from_lot_id`, and keeps the supplier, dates, code
and cost. Batch facts stay immutable. The kept product's average cost becomes the
quantity-weighted average of the two. Weight labels from scales move stock by the weighed
quantity; price labels move one labelled pack. See
[PRICING_AND_CATALOGUE.md](PRICING_AND_CATALOGUE.md).

## Bundles and stock (Wave 6)

A bundle, kit or hamper is virtual: it is made when sold and has no stock of its own (stock
tracking is switched off when it is set up, and a product with recorded stock cannot become one).
- A sale takes each component from stock in the same commit, as ordinary `sale` movements with
  each component's average cost. The bundle never moves.
- The negative-stock rule applies to the components and names them.
- Availability is the smallest whole number of bundles the components' stock can make, with the
  limiting item.
- Refunds are of whole bundles and return each component to stock.
- Replenishment and stock cover work on the components. Nothing is manufactured in advance.
- Details: [PROMOTIONS_AND_BUNDLES.md](PROMOTIONS_AND_BUNDLES.md) section 4.

## Wave 8 interactions

- Delivery notes and waste or stocktake photos can be kept in the Document Library and linked
  to the product, supplier, purchase order or supplier return. Nothing in the library moves
  stock: links point at records and copy no quantity.
- Business Memory may hold facts about products ("sells out before Eid"). They never change
  reorder points or suggestions, and the assistant shows them below the records.
