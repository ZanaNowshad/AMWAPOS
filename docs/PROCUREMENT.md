# Procurement: from demand to the supplier's credit (Wave 4)

Plan: [MERCHANT_OS_PLAN.md](MERCHANT_OS_PLAN.md). Stock and batches:
[INVENTORY.md](INVENTORY.md). Payables: [FINANCE.md](FINANCE.md).

The loop:

> demand → days of stock → suggested order → requisition → approval →
> purchase order → receiving → differences → supplier document →
> three-way match → payable → supplier return → credit.

Every step is a person's decision. The app works out the numbers and shows
its evidence; it never orders, approves, receives, accepts a difference,
posts an invoice or confirms a return by itself, and neither does the AI.

## Supplier catalogue

One model with two kinds of record (`catalogue.rs`):

| Record | Cardinality | Holds |
| --- | --- | --- |
| `supplier_products` (new) | one per supplier and product | the terms the product is ordered on: the supplier's item code, units in one pack, minimum order (in packs), lead time (days), preferred, active |
| `supplier_product_map` (unchanged) | many per supplier and product | how the supplier's documents name the product: item codes and normalized descriptions, with the pack size seen there |

`supplier_product_map` is keyed by supplier and document key, so one
product can have several aliases. It stays exactly as Document
Intelligence uses it; its ids and behaviour are unchanged.

**Who wins.** A pack size has a source:
- `person`: typed or confirmed on Suppliers → (supplier) → Ordering terms;
- `document`: confirmed while reviewing a supplier document.

A person's pack size wins. Document learning fills an empty pack size and
never replaces a person's. When a supplier document prints a different
pack size for a product whose pack a person confirmed, the line uses the
person's pack size and is flagged (`document_pack_differs`).

**Pack arithmetic** (`catalogue::order_quantity`) is exact integer
arithmetic in milli-units:
- a need is rounded up to whole packs, then raised to the minimum order;
- a case of 24, a need of 37, a minimum of 2 cases → 2 cases = 48 units;
- with no pack size: whole units (exact for products sold by weight), and
  the minimum is in units;
- a pack size or minimum of zero or less is refused (by the API, the
  function and a CHECK constraint).

One preferred supplier per product (a partial unique index).

**Cost baselines** (`catalogue::cost_baselines`):
- order cost: the purchase order line;
- last confirmed cost: the newest cost a person confirmed from that
  supplier, from a receipt or a posted invoice (`product_cost_history`
  sources `receiving` and `supplier_invoice`) — never a figure read by OCR
  alone;
- typical cost: the median unit cost of the last five receipts from that
  supplier (the lower middle one for an even count).

## Replenishment (Suggested orders)

One deterministic engine (`replenish.rs`). It reads every product in a
fixed number of grouped queries — stock levels, holds, open and draft
purchase orders, transfers on the way, requisitions, waste, demand, terms,
last costs — and decides each product with a pure function (`assess`).
The only per-product work is the batch replay, and only for products with
a dated batch within the horizon.

**Demand** is the Wave 3 definition, now shared and set-based
(`lots::demand_all`, `lots::demand` for one product): net units sold
(sales less refunds and voids, each on its own business date) over the
demand window (Settings → Purchasing, 7/30/60/90 days), from at least 7
days of history. Days of stock left (Wave 3) uses the same functions.

**Stock position — nothing counted twice:**

| | |
| --- | --- |
| usable | on hand − active order holds − expired **use-by** stock |
| on the way | ordered and not yet received or cancelled (`ordered`, `partially_received`) + draft purchase orders + shipped transfers into the branch + requisition lines not yet on an order |
| position | usable + on the way |

Best-before stock past its date is still usable; use-by stock is not. A
requisition line stops counting the moment it is on a purchase order, and
the order line counts instead.

**Levels:**
- reorder point = the product's reorder point when set, else
  daily demand × (lead time + safety days);
- order up to = the product's maximum stock when set, else daily demand ×
  (lead time + safety days + days between orders), never below the
  reorder point.

An order is suggested when position ≤ reorder point (strictly below when
there is nothing to fill above the reorder point), for
(order up to − position), rounded with the pack arithmetic. All divisions
round up, in integers.

**States** (each row has exactly one):

| State | Meaning |
| --- | --- |
| `order` | order the suggested quantity |
| `covered` | position is above the reorder point |
| `insufficient_history` | under 7 days of history and no product levels |
| `no_demand` | no net sales in the window and no product levels |
| `no_supplier` | an order is needed but no active supplier terms exist |
| `no_lead_time` | demand-based levels need the supplier's lead time |
| `inactive` | the product is inactive |
| `invalid_pack` | the pack arithmetic refused the terms |

**Reason codes** explain the figures: `expired_stock_not_counted`,
`holds_not_counted`, `on_order`, `transfer_on_the_way`,
`already_requested`, `at_or_below_reorder_point`, `rounded_to_packs`,
`minimum_order`, `insufficient_history_using_product_levels`,
`no_demand_using_product_levels`. **Warnings** never change the quantity:
`expiry_risk` (stock expiring before the next delivery could be used),
`high_waste` (waste in the window above 10 % of net sales),
`no_lead_time`.

**Supplier selection** is deterministic: preferred first, then a known
lead time, then the lowest last confirmed cost, then the shortest lead
time, then the name and id. The reason is shown (`preferred_supplier`,
`only_supplier`, `has_lead_time`, `lowest_last_cost`,
`shortest_lead_time`, `first_by_name`) and every other supplier is listed
as an alternative with its terms and last cost.

**Nothing is ordered.** "Create requisition" sends product ids only; the
server recomputes the suggestions at that moment and stores each line's
evidence (the facts, levels, reasons and alternatives).

## Requisitions

```
draft ─submit→ submitted ─approve→ approved ─convert→ converted
  │                └─reject (with a reason)→ rejected
  └─ draft | submitted | approved ─cancel→ cancelled
```

- `requisitions.create` creates, edits (draft only) and submits;
  `purchasing.approve` approves or rejects (a reason is required);
  `purchasing.manage` converts.
- Every line keeps where it came from: `manual` (typed by a person) or
  `replenishment` (with the engine's evidence). A suggested line whose
  quantity or supplier a person changes keeps its source and is marked
  `edited`.
- Submitting needs a supplier on every line.
- **Conversion** turns the whole requisition into draft purchase orders,
  one per supplier, in one transaction, exactly once:
  - the same operation id returns the same orders; another conversion is
    refused (`already_converted`); four concurrent conversions make one
    set of orders (tested);
  - a line without a cost uses the last confirmed cost from that
    supplier, or the conversion is refused and names the line;
  - an inactive supplier refuses the conversion and nothing is created;
  - **partial conversion is refused**: to order part, cancel and create a
    new requisition.
- Each order line keeps `requisition_line_id`; each requisition line keeps
  its `po_id` and `po_item_id`; the order keeps `requisition_id`.

## Purchase order approval

Settings → Purchasing → Purchase order approval: **off** (the default,
ordering exactly as before), **every purchase order**, or **above an
amount** (the order total).

There is still **one status system**: draft → ordered → partially received
→ received, or cancelled. Approval is a property of a draft, not a status:
a draft shows "Needs approval" or "Approved". Rebuilding the status CHECK
to add approval states would mean rebuilding `purchase_orders`; the
property gives the same control without it.

**Durable approvals** (`purchase_order_approvals`): who, when, the
order's version and total, the policy in force, a note and the operation
id. Not the 120-second Wave 1 tokens.

An approval covers a **fingerprint** of the material details: supplier,
every line's product, quantity, cost, tax and total, and the order
totals. A note, the reference or the expected date are not material. A
material edit invalidates the approval (kept in the history with
`invalidated_reason = edited`, audited as `po.approval_invalidated`), and
placing the order is refused until it is approved again
(`approval_required`). The policy is applied when the order is placed.

## Receiving

Purchase orders → (order) → Receive goods. For each line:

| Field | Meaning |
| --- | --- |
| Still expected | ordered − received − cancelled |
| Accepted | becomes stock (with the batch and expiry, as in Wave 3) |
| Refused | goes back with the driver, with a reason (damaged, wrong item, short-dated, expired, quality, not ordered, other) |
| Damaged, kept | part of the accepted quantity that is damaged but kept |
| Substitute | product B delivered instead of the ordered A |
| If short | keep the rest on order (backorder) or cancel it |

- **Only accepted quantities create stock.** Refused goods never become
  stock and are never recorded as waste (tested). They are not a supplier
  return either: they never entered stock.
- **Shortage** stays visible as a `receipt_discrepancies` row: `open` until
  a person keeps it on order (`backorder`) or cancels it (the line's
  `qty_cancelled_milli` grows). A later delivery settles earlier open
  shortages. Closing an order short cancels what remains, visibly.
- **Overage** must be confirmed on the line ("Keep the extra"); beyond the
  quantity tolerance (Settings → Purchasing, default 0 %) it also needs
  `purchasing.approve`, bound to that exact delivery (Wave 1 approval
  binding). It is recorded with who approved it.
- **Substitution** keeps the link (ordered A → received B): stock goes to
  B, the order line A counts it, the receipt line records
  `substitute_for_product_id`. It must be accepted explicitly.
- Delivered (accepted + refused) is kept on the receipt line
  (`qty_delivered_milli`; NULL for receipts made before Wave 4).
- **Cost differences never block receiving.** They are reviewed in the
  three-way match before the invoice is posted.
- **Idempotency:** the operation id makes a retry (a lost reply after the
  commit) change nothing; the same id with a changed or partial request is
  refused (`idempotency_mismatch`); a failure part way (an impossible batch
  date on a line) writes nothing; four people receiving the same order at
  once cannot receive it twice (all tested).

## Three-way match

The Document Intelligence `reconcile` is the one matcher — there is no
second one. It compares, line by line:

- **ordered** (the purchase order line);
- **received**: everything accepted into stock against the line, over all
  receipts;
- **invoiced**: this document plus the supplier's invoices already
  **posted** for the same order (cumulative), so the same goods are never
  owed twice. A document's own invoice record is excluded.

Quantities are in single units: a document line printed in cases is
converted with its pack size first (a person-confirmed pack size wins).
Supplier invoice records created from documents are now stored in single
units with the per-unit cost.

| Line result | When |
| --- | --- |
| matched | quantities and cost agree (a line ordered but not invoiced yet is matched) |
| within tolerance | the cost differs within both limits |
| review | a cost beyond either limit, a line not on the order, a line not matched to a product, or a VAT rate different from the order |
| blocked | invoiced (cumulative) more than was accepted |

The document's outcome is the worst line's. A cost difference is within
tolerance when it is within **both** limits (Settings → Purchasing): the
line's value difference (fils) and the unit cost difference (basis
points). The match shows the order cost, the invoice cost, the last
confirmed cost and the typical cost for every line.

**Payables gate.** An invoice for a purchase order is posted only when its
match is matched or within tolerance, or when someone with
`purchasing.approve` accepted a match that needs review, with a reason.
The acceptance covers the fingerprint of the result it saw; if the
evidence changes (another receipt, another posted invoice), it no longer
applies. A blocked match cannot be accepted. Nothing is posted
automatically; the match is never decided by AI.

The cost tolerance had one home before (Inventory → supplier documents);
the upgrade copies it to Purchasing, which now serves documents and
invoices alike.

## Supplier returns

```
draft ─confirm→ confirmed ─credit note posted→ credited
  └─cancel→ cancelled          confirmed ─reverse→ reversed
```

- Reasons: damaged, incorrect item, over-delivery, short-dated, expired,
  quality, recalled, commercial agreement, other.
- **Confirming** writes exactly one `supplier_return` movement per line,
  out of the batch when the line names one (evidence, as Wave 3 waste), in
  the same transaction as the checks:
  - not more than is on hand, or than the batch has left;
  - with a receipt: not more than it accepted, less what was already
    returned from it.
- A retry with the same operation id writes nothing; another confirmation
  is refused.
- **Reversal** (confirmed, not credited) writes compensating movements back
  into the same batch; the return stays, marked reversed.
- **Expected credit** = quantity × the cost the goods came in at (the
  batch's, else the receipt's, else the average cost). The **actual
  credit** is the supplier's credit note, a Payables record: drafted from
  the return or linked to one already entered; one credit note per return
  and one return per credit note (unique). Posting it marks the return
  credited; reversing or voiding it frees the return. **The credit note
  moves no stock** — the return already did.

## Accounts payable

Payables is reused as it is: liabilities and credits are created only by
an explicit, authorized posting, once (`invoice_id` unique on both).
Wave 4 adds the match gate above, the purchase order on manual invoices
(`po_id`), the VAT rate on manual lines, and the return ↔ credit note
link.

## Settings (Settings → Purchasing)

| Setting | Default |
| --- | --- |
| Safety stock (days) | 3 |
| Days between orders | 7 |
| Measure sales over | 30 days |
| Purchase order approval | off |
| Orders that need approval: above | BHD 100.000 |
| Extra delivered without a manager | 0 % |
| Cost difference accepted on a line | BHD 0.500 |
| Cost difference accepted per unit | 5 % |

Per-product: reorder point (product form, as before) and maximum stock
(product → Inventory → Ordering). Per supplier and product: the ordering
terms.

## Permissions

| Permission | Owner | Manager | Inventory | Allows |
| --- | --- | --- | --- | --- |
| `requisitions.create` (new) | ✓ | ✓ | ✓ | create, edit and submit requisitions |
| `purchasing.approve` (new) | ✓ | ✓ | | approve requisitions and purchase orders, over-deliveries beyond tolerance, invoice matches that need review |
| `supplier_returns.manage` (new) | ✓ | ✓ | ✓ | prepare, confirm and reverse supplier returns; record the credit note |
| `suppliers.manage` (existing) | | | | supplier terms |
| `purchasing.manage` (existing) | | | | purchase orders, conversion, shortage decisions |
| `inventory.receive` (existing) | | | | receiving |
| `payables.*` (existing) | | | | posting stays as it was |

Cashiers have none of these. Upgrades add the new ones to built-in roles
once and never re-add one the owner removed (tested).

## Ownership

| Table | Where |
| --- | --- |
| `supplier_products`, `requisitions`, `requisition_lines`, `purchase_order_approvals`, `receipt_discrepancies`, `supplier_returns`, `supplier_return_lines` | Hub only (`sync::LOCAL_TABLES`); a till refuses every write (`require_back_office_writable`) |
| `products.max_stock_milli` | with the product (hub-owned) |
| The stock movements of receiving and returns | replicate as before |

## AI

The AI can read: suggested orders (`suggested_orders`), the supplier
catalogue, requisitions, open shortages, the invoice match and supplier
returns. "Reorder" proposes a **requisition** (not an order) built from
the engine's suggestions, recomputed when a person confirms it; the old
reorder arithmetic (2 × reorder point − on hand − on order) is gone and
`ai.reorder_suggestions` reads the engine.

It cannot approve, order, receive, decide a shortage, accept a match,
post, confirm or reverse a return, create or link credits, or change
supplier terms or stock levels: those commands are in `NO_TOOL` and are
classed as financial or inventory commits. The tools that proposed
placing (`propose_po_status`) and receiving (`propose_po_receive`) a
purchase order were removed.

## Migration (0030)

Creates nothing that did not happen:
- `supplier_products` rows only for supplier–product pairs seen in
  document mappings, purchase orders or receipts; a pack size only where
  the documents confirmed exactly one (as `document`); no minimum, lead
  time or preferred supplier;
- no requisitions, approvals, differences or returns;
- draft purchase orders stay drafts and are not approved; old orders and
  receipts stay valid (`qty_cancelled_milli` 0, `qty_delivered_milli`
  NULL).

The upgrade test builds a schema-29 store and checks all of this.

## Tests

- `tests/wave4.rs` (20): terms and precedence; suggestions counting stock
  once and never ordering; explicit states; requisition lifecycle and
  single conversion; concurrent conversions; a failed conversion; approval
  durability and invalidation; receiving differences; backorder;
  overage confirmation and approval; retry / changed / partial /
  failure-part-way receiving; concurrent receiving; batches at receiving;
  the match (blocked, within tolerance, cumulative, review, acceptance,
  VAT, not on the order); returns (one movement, batch evidence, credit,
  no second stock effect); reversal; a return racing sales; the upgrade
  from schema 29; permissions and owner removals.
- `catalogue.rs`, `replenish.rs` unit tests: pack arithmetic and refusals;
  every engine state; double counting; supplier ranking; waste as a
  warning only.
- `crates/amwapos-hub/tests/ai_hardening.rs`: reorder reads the engine and
  only records a requisition proposal.
- `tests/perf.rs`: Suggested orders over 100,000 products.
- `e2e/wave4.spec.ts`: Suggested orders → requisition → approve → purchase
  order → place → receive with a refusal and a shortage → differences →
  supplier return; `e2e/surfaces.spec.ts` sweeps the new pages in English
  and Arabic.

## Not proven here (external)

- Real supplier terms, lead times and minimums in Bahrain.
- Real delivery checking at the back door (refusals with the driver,
  substitutions agreed by phone).
- Real supplier credit notes and how long they take.
- The screens on the 1024×768 back-office panel with real catalogues.
