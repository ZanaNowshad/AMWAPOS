# Merchant operating system — architecture plan

Status: **proposal, before implementation.** Written from the repository at
`main` = `11d29e3` (schema 26, 67 permissions, 295 commands). Nothing here is
built yet. Each wave below ends in its own checkpoint and updates
STATUS / ARCHITECTURE / OPERATIONS / UI.

The operating pattern stays the one AMWAPOS already follows:
real-world input → structured evidence → deterministic logic → human authority
→ authoritative transaction → reconciliation → durable knowledge.

---

## A. Current-state reuse map

Legend: **REUSE** as-is · **EXTEND** · **REFACTOR** locally · **REPLACE** only if
necessary · **NEW** (not present).

### Foundations every program leans on

| Mechanism | Where | Verdict |
|---|---|---|
| Integer fils money, `mulDivRound` half-away-from-zero | `money.rs`, `pricing.rs`, vitest | REUSE — every new amount is `*_minor` i64 |
| Idempotency: `operation_id` UNIQUE on records + `operation_idempotency` (payload hash, replayed result) | `idempotency.rs` | REUSE for every new consequential command |
| Hash-chained audit `audit::record` | `audit.rs` | REUSE |
| Sync ownership `Policy::{Hub, Append, Shared}` + non-replicated back office guarded by `require_back_office_writable` | `sync.rs`, `service.rs` | EXTEND (new tables classified in F) |
| Permissions + role seeds respecting customer removal (`role_permission_seeds`, `UPGRADE_PERMISSIONS`) | `auth.rs`, migration 0013 | EXTEND |
| Manager approval: single-use, 120 s, bound to one permission | `auth.rs` `issue/consume_approval` | EXTEND (Program Y binding) |
| Feature flags | `settings.rs` `features` | REUSE to keep new domains opt-in |
| `time::business_date(t, tz)` — one central function; `business_date` already stored on sales, refunds, shifts | `time.rs` | EXTEND with cutoff |
| Append-only ledgers with no-update/no-delete triggers (customer_ledger, loyalty_ledger, ap_*) | migrations 0006/0009/0026 | REUSE the pattern |
| `ai_actions.rs` action classes (read … financial) | AI | EXTEND with new commands |
| Deterministic reports registry (`reports.rs` 15 reports) + EOD pack | `reports.rs`, `eod.rs` | EXTEND |

### Per program

| Program | Exists today | Verdict | Must not duplicate |
|---|---|---|---|
| A Expenses | Only `cash_events.paid_out` with a free-text reason; AP is for supplier invoices | NEW `expenses` ledger; EXTEND `cash_events` with an optional `expense_id` link | AP (supplier invoices stay in AP; an expense may *reference* a supplier but is not a payable) |
| A Petty cash | Nothing separate from the till drawer | NEW `petty_cash_funds` + `petty_cash_entries` (append-only) | Must not reuse sales `cash_events` for non-till money |
| A Operating profit | Gross margin per product/category exists (`margin` report, `cost_snapshot_minor`) | EXTEND reports with `operating_profit` | COGS stays `sale_items.cost_snapshot_minor` − refund costs; no second COGS |
| B Business date / cutoff | Calendar date in store tz, stored at commit | EXTEND `business_date()` with `cutoff_minutes` | No per-report cutoff logic |
| B Opening checklist | Backup banner, diagnostics, start-shift gate | NEW thin aggregator over existing health checks | Diagnostics checks (reuse their functions) |
| B X report | `shifts` report, EOD pack, tender report | EXTEND: X = deterministic read over the business date | — |
| B Z report | None (EOD is a view, not a close) | NEW `day_closes` (immutable snapshot JSON + hash) | EOD pack stays the read view; Z freezes it |
| C Register / drawer / session | `shifts` is device-bound (`ux_shift_open_device`) | EXTEND: add `registers`, `cash_drawers`; `shifts` gains nullable `register_id`/`drawer_id` and stays the cash session | Do not create a second "session" table beside `shifts` |
| C Variance cases | `shifts.variance_approved_by` | NEW `cash_cases` (generic case, also used by alerts) | — |
| D Lots / expiry / FEFO | None | NEW `stock_lots`, `stock_movements.lot_id` | Stock level stays derived from movements; lots never become a second quantity truth |
| E Waste | `inventory_adjust` with a reason | NEW movement type `waste` + `waste_records` header | Do not hide waste inside `adjust` |
| F Replenishment / days of cover | `reorder_point_milli`, low-stock list, AI "reorder" helper (B4) | NEW deterministic `replenishment.rs`; AI helper becomes a reader of it | The AI helper's own arithmetic is retired |
| G Supplier catalogue | `supplier_product_map` (DI learned mappings: code/desc → product, `units_per_case`), `product_cost_history` | EXTEND `supplier_product_map` into the catalogue (add sku, barcode, moq, lead_time_days, preferred, active, last cost/dates) | No second mapping table |
| H Requisitions | None | NEW `requisitions` (+ lines) | — |
| I PO approval | PO `draft → ordered → partially_received → received / cancelled` | EXTEND status with optional `submitted`, `approved`, `rejected` (policy-gated) | — |
| J Receiving discrepancies / 3-way match | `goods_receipts` + DI `checks::reconcile` (invoice vs PO), receiving drafts | EXTEND: `goods_receipt_items` gains discrepancy columns; NEW `receipt_discrepancies`; `reconcile` becomes PO ⇄ receipt ⇄ invoice | No parallel matcher |
| J Cost variance | DI price-variance threshold setting (`inventory` settings, P75) | EXTEND into policy with abs/% vs PO/last/typical | — |
| K Supplier returns | None (credit notes exist in AP) | NEW `supplier_returns` → stock movement `supplier_return` + optional AP credit-note link | AP credit-note machinery reused |
| L Receipt snapshots / hash | Receipts rebuilt from committed lines **but header/footer/business/branch come from current settings** (`receipt.rs:397`) | NEW `receipt_snapshots` written at commit | — |
| M Completed-sale void | Not present; `sales.status CHECK IN ('completed')` | NEW `sale_voids` (append) — sale row untouched | Refund engine reused for stock/tender reversal maths |
| N PLU / scale barcodes | `product_barcodes(source)`; no PLU | EXTEND `product_barcodes.kind`; NEW `scale_barcode_rules` | — |
| O Duplicates / merge | Unknown-barcode "merge" into an existing product only | NEW duplicate finder + `product_merges` record | — |
| P Bahrain address | Done for flat/building/road/block/landmark (0018) on customers, deliveries, digital orders, addresses | EXTEND with `governorate`, `area` normalisation, `directions` | `address.rs` stays the one parser |
| Q Credit ageing / statements | `customer_ledger` (immutable), balance, limit, payments; PDF engine exists | EXTEND: FIFO ageing over the ledger + statement report/PDF | — |
| R Sales channel | `digital_orders.channel`, `delivery_orders.channel`; sales have none | EXTEND `sales.channel` (nullable = unknown for history) | — |
| R Channel pricing | `product_prices.price_type` (only `retail` used), branch prices | EXTEND resolver: `price_type` = channel price | No duplicated product rows |
| S Pricing policy / rounding / margin | AI B5 price helper (cost ÷ (1−margin)), DI cost-variance alert | NEW `pricing_policy.rs` deterministic; AI explains it | — |
| T–U Promotions / coupons / bundles | None | NEW, later (wave 6), as a stage of `price_cart` | — |
| V Alert centre | `ai_alerts` (5 kinds, dismiss only), Dashboard "Needs attention" | REFACTOR `ai_alerts` → `alerts` with states, assignee, notes | Dashboard reads the same table |
| W Sync reconciliation | `sync_dead_letters` (open/resolved, retry) + UI | EXTEND states, reasons, safe actions | Protocol unchanged |
| X Terminal health | `device_heartbeats` (last seen, version, schema, pending, errors, user) | EXTEND (active shift, backup age, protocol) | Never invent printer state |
| Y Approval binding | Permission-bound single-use token | EXTEND with action, entity, payload digest, op id, device | Low-risk overrides unchanged |
| Z Credential rotation | `devices.credential_hash`; "Reset hub credentials" resets *all* | EXTEND per-device `credential_version`, rotate one | — |
| Jobs | Separate workers: OCR, DI, images, orders, WhatsApp, catalogue, backup, sync | REUSE; extract a shared `jobs` table only when a new long job needs it (lot FEFO consumer, replenishment batch) | No rewrite of working workers |
| Document library | DI (`invoice_scans`) + AI attachments | Later (wave 8) | — |

---

## B. Domain dependency graph

```
                    business date (cutoff) ─────────────┐
                           │                             │
 receipt snapshot ◄── SALE ──► sale void ◄── approval binding
      │                    │  channel ──► channel pricing ──► promotions/bundles
      │                    ▼
      │            stock movement (+lot) ◄── lots ◄── receiving ◄── PO ◄── requisition ◄── replenishment
      │                    │                    │          │        ▲                         ▲
      │                    ▼                    ▼          ▼        │                         │
      │                  waste            expiry/FEFO   discrepancy │                 days of cover
      │                    │                    │          │        │                         │
      │                    └────────► supplier performance ◄─ 3-way match ◄─ supplier invoice (DI) ─► AP
      │                                                                                   ▲
 shift = cash session ◄── register/drawer          supplier return ─► AP credit note ─────┘
      │        │
      │   cash case (variance)
      ▼
   X report ──► EOD control ──► Z close (immutable) ◄── expenses ◄── petty cash
                                        │
                     gross profit − operating expenses = operating profit
                                        │
                    alerts (one centre) ──► Dashboard ──► AI explanations
```

Hard dependencies: business-date cutoff before X/Z and before expenses carry a
business date; approval binding before sale void; one stock-movement schema
rebuild before waste, lots, supplier returns and voids; supplier catalogue
before replenishment chooses suppliers; receiving discrepancies before 3-way
match.

---

## C. Proposed schema

All money `INTEGER *_minor`, quantities `*_milli`, ids ULID `TEXT`, times UTC
`TEXT`, `business_date TEXT` (store-local trading day). Every financial table
gets no-update/no-delete triggers unless it is a state-machine header whose
transitions are audited.

### Wave 1
- `expense_categories(category_id, name, name_ar, active, sort)` — seeded defaults, editable.
- `expenses(expense_id, number, branch_id, business_date, category_id, payee_supplier_id?, payee_text?, description, amount_minor, vat_treatment[none|inclusive|exclusive|exempt], vat_minor, total_minor, payment_method?, cash_event_id?, petty_fund_id?, status[draft|submitted|approved|rejected|paid|void], recurring_id?, created_by, submitted_by/at, decided_by/at, decision_note, paid_by/at, paid_operation_id UNIQUE?, void_of?/void_reason, revision, created_at, updated_at)` — header with audited transitions. Paid/void are terminal.
- `expense_attachments(attachment_id, expense_id, path, sha256, mime, added_by, added_at)` — reuse DI file storage.
- `expense_recurring(recurring_id, template_json, cadence[monthly|weekly], next_date, active)` → generates **drafts** only.
- `petty_cash_funds(fund_id, branch_id, name, custodian_user_id, active)`.
- `petty_cash_entries(entry_id, fund_id, kind[open|top_up|expense|reimburse|adjust|count], amount_minor (signed), expense_id?, counted_minor?, note, operation_id UNIQUE, user_id, business_date, created_at)` — append-only; balance derived.
- `receipt_snapshots(sale_id|refund_id|void_id PK, kind, canonical_json, sha256, format_version, created_at)` — append-only, written in the same transaction as the sale.
- `sale_voids(void_id, void_number, sale_id UNIQUE, reason, approved_by, user_id, device_id, shift_id, business_date, operation_id UNIQUE, created_at)` + `sale_void_tenders` — append-only.
- `customer_ledger` unchanged; ageing derived (FIFO) — no new table.

### Wave 2 (as built; see FINANCE.md "Closing the trading day")
- The trading-day cutoff is Wave 1's (`shift.day_cutoff_minutes`, `time::business_date`); Wave 2 adds no second calculation and never recomputes a stored business date.
- `day_closes(close_id, close_number, branch_id, business_date, format_version, snapshot_json, sha256, totals…, closed_by, closed_at, operation_id UNIQUE)`, UNIQUE(branch_id, business_date), immutable. No reopen: a correction is never a rewrite; records that arrive late go into the next close.
- `day_close_items(ref_kind[sale|refund|shift], ref_id, close_id, business_date, late)`, PK(ref_kind, ref_id): each record is counted by exactly one close.
- `registers(register_id, branch_id, code, name, active, device_id, default_drawer_id)`; `cash_drawers(drawer_id, branch_id, register_id, name, active)`; `shifts` + nullable `register_id`, `drawer_id`. The shift stays the cash session.
- `cases(… kind[cash_variance], severity, status[new|acknowledged|in_progress|resolved|dismissed], entity, facts_json …)` + `case_events` (append-only). "investigating" was named `in_progress`.

### Wave 3 (as built; see INVENTORY.md)
- No `stock_movements` rebuild: Wave 1 already added `waste`, `supplier_return`, `void` and `lot_id`.
- `stock_lots(lot_id, lot_number, product, branch, location?, supplier?, po?, receipt?, supplier_lot_code?, received_at, manufactured_on?, expires_on?, expiry_kind[use_by|best_before], expiry_source[person|document], qty_received_milli, unit_cost_minor, provenance[receiving|count])`: facts only, immutable. There is no stored remaining quantity and no lot status: both are derived (replay; expiry state from the date).
- `lot_corrections` (append-only, the newest applies); `waste_records(…, reason[expired|damaged|spoiled|broken|shrinkage|internal_use|receiving_rejection|other], movement_id, status[recorded|reversed], …)`.
- Settings in `inventory`: expiry thresholds, waste approval value, shrinkage approval.
- No FEFO allocation table: the estimate is computed on read (deterministic and order-independent), so it is never stored as evidence.

### Wave 4 (as built; see PROCUREMENT.md)
- `supplier_product_map` unchanged (document aliases, many per product). NEW `supplier_products(supplier_id, product_id PK, supplier_code, units_per_case CHECK > 0, pack_source[person|document], moq_packs CHECK > 0, lead_time_days, preferred (one per product), active, terms_confirmed_by/at, version)` for the orderable terms; seeded from facts only. Last/typical costs are derived (cost history, receipts), not stored.
- `products.max_stock_milli` (optional order-up-to).
- `requisitions(requisition_id, number, branch_id, status[draft|submitted|approved|rejected|cancelled|converted], note, created/submitted/decided/converted by+at, decision_note, convert_operation_id UNIQUE, create_operation_id UNIQUE, version)` + `requisition_lines(…, source[manual|replenishment], evidence_json, po_id, po_item_id)`.
- No new PO status (rebuilding the CHECK was avoided): approval is a durable `purchase_order_approvals(approval_id, po_id, po_version, fingerprint, total_minor, policy_mode, threshold_minor, approved_by/at, note, invalidated_at/reason, operation_id UNIQUE)` that covers a fingerprint of the material details; `purchase_orders.requisition_id`; `purchase_order_items.requisition_line_id, qty_cancelled_milli`. Settings key `purchasing`.
- `goods_receipt_items` + `qty_delivered_milli`, `substitute_for_product_id`; NEW `receipt_discrepancies(…, kind[shortage|overage|damaged|rejected|substitution], qty_milli, reason, resolution[open|backorder|cancelled|accepted|rejected], approved_by, resolved_by/at)`. Cost differences are computed in the match, not stored as discrepancies.
- `supplier_invoices` + `match_accepted_by/at/fingerprint/note`.
- `supplier_returns(return_id, number, supplier_id, branch_id, receipt_id?, po_id?, status[draft|confirmed|credited|cancelled|reversed], expected_credit_minor, credit_invoice_id UNIQUE, confirm/reverse operation ids UNIQUE)` + `supplier_return_lines(…, lot_id?, qty_milli, unit_cost_minor, reason, movement_id, reversal_movement_id)`.

### Wave 5
- `product_barcodes.kind[ean13|ean8|upc|code128|internal|plu|supplier]`.
- `scale_barcode_rules(rule_id, name, prefix, length, plu_start/len, value_kind[weight|price], value_start/len, decimals, check_digit[none|ean|price_cd], active, priority)`.
- `product_merges(merge_id, source_product_id, target_product_id, preview_json, moved_json, user_id, at)`.
- addresses + `governorate`, `directions` on the existing address-bearing tables.
- `sales.channel` (nullable; `pos` for new till sales, NULL = unknown history), `carts.channel`.
- `product_prices.price_type` values `retail|delivery|whatsapp|web` (resolver falls back to `retail`).
- `pricing_policies(policy_id, scope[global|category|supplier|branch|channel], scope_id, markup_bp?, target_margin_bp?, min_margin_bp?, rounding_step_minor, ending_minor?, priority)`.

### Wave 6–8
`promotions`, `promotion_rules`, `coupons`, `bundles`/`bundle_components`,
`alerts` (refactor of `ai_alerts` into `cases`-backed work items),
`sync_dead_letters` + `state`, `reason_code`, `resolution`, `device_heartbeats` +
fields, `devices.credential_version`, optional `jobs`, `documents` library.

---

## D. State machines

Every transition: permission-checked, audited (`who/what/when/why/op id`),
idempotent on `operation_id`, refused with a plain message when illegal.

**Expense**
```
draft ─submit→ submitted ─approve→ approved ─pay→ paid
  │               └─reject→ rejected (terminal; copy to new draft to retry)
  └─delete (draft only, never audited money)
approved|paid ─void(reason)→ void   (paid void writes a compensating petty/cash entry, never edits the original)
```
Policy: merchants without approval set `expenses.approval = off` → `submit` auto-approves (recorded as such).

**Petty cash fund:** no states; entries append; `count` entry records counted vs derived balance → variance case if over threshold.

**Requisition**
```
draft → submitted → approved → converted (PO created, req_id on PO)
              └→ rejected        draft|submitted → cancelled
```

**Purchase order (as built: one status system, approval as a property of the draft)**
```
draft ──(approval valid, when the policy needs it)──→ ordered → partially_received → received
  └────────────→ cancelled  (only before any receipt)        └─ close short (rest cancelled)
```
Approval off ⇒ `draft → ordered` exactly as before. A material edit voids the approval.

**Receiving**: a goods receipt is a one-shot append document. Differences
are separate rows: shortage `open → backorder | cancelled`; overage / damaged kept /
substitution `accepted`; refused `rejected` (never stock, never waste).

**Supplier return**
```
draft → confirmed (stock leaves: movement supplier_return) → credited (AP credit note posted)
  └→ cancelled (draft only)     confirmed → reversed (compensating movements)
```

**Cash variance case / alert (shared `cases`)**
```
new → acknowledged → investigating → resolved(reason)
  └────────────────────────────────→ dismissed(reason)
```
History is `case_events`, append-only. Wording is evidence-led ("Drawer
counted 2.500 short") — never accusatory.

**Z close**
```
(open day) ─close→ closed  [snapshot + hash frozen]
closed ─reopen(owner, reason)→ reopened ─close→ closed (new row, supersedes)
```
A closed day's snapshot is never recomputed; a reopen produces a new close.

**Sale void**
```
completed sale ─void(reason, bound approval)→ voided-by(sale_void row)
```
Rules: same business date, same branch, not yet in a closed Z, no refund
against it, no delivery collection settled. Otherwise use a refund.

---

## E. Invariants

Money and accounting
1. All money is integer fils; no float anywhere on a money path.
2. A posted/paid/closed record is never updated in its financial fields; corrections are reversals that reference the original.
3. Operating profit = Revenue − COGS − Approved+Paid operating expenses for the business-date range; "Net profit" is not used.
4. Expense VAT: `total = amount + vat` (exclusive) or `vat` derived from inclusive total with the store rounding; recomputed on the backend only.
5. Petty-cash balance = Σ entries; never stored as an editable number.
6. Customer ageing buckets sum to the ledger balance exactly; ageing uses the *charge* date with FIFO application of payments/credits.
7. AP balance (existing invariant) unchanged; supplier returns reduce it only through a posted credit note.

Stock
8. Stock level = Σ movements (existing); lots never hold an independent quantity: lot remaining = Σ movements carrying that `lot_id`; Σ lots ≤ branch stock, the rest is "not lotted".
9. No movement without a type and source document; waste, returns and voids are movements, never level edits.
10. No stock is created by bundles, transfers or merges.
11. FEFO allocation is advisory/derived where the store cannot identify the lot sold; it never overrides a counted lot.

Time and closing
12. `business_date` is computed once, at commit, by `time::business_date(t, tz, cutoff)`; the original UTC timestamp is kept; changing cutoff never rewrites history.
13. A Z close freezes its snapshot JSON + SHA-256; reports for a closed date read the snapshot for "as closed" and live data for "current", and say which.
14. No new sale, refund, void or cash event can carry a business date that has a closed Z on that branch (the till moves to the next trading day; a late-syncing terminal record lands in an "after close" adjustment line on the next Z, never silently into the closed one).

Receipts
15. The issued receipt is reproducible byte-for-byte from `receipt_snapshots`; reprints add "COPY" outside the hashed body.

Sync
16. Every new table has exactly one policy (F). Append tables are insert-only and idempotent; hub-owned writes refuse on a terminal.
17. Offline selling never waits on lots, promotions requiring hub data, expenses, Z, alerts, AI or WhatsApp.

Authority
18. AI may draft expenses, requisitions, markdown proposals, explanations; it never posts, approves, pays, closes, voids or changes price.
19. A high-risk approval is valid for one action, one entity, one payload digest, one device, once, before expiry.

---

## F. Hub / terminal ownership matrix

| Data | Policy | Created where | Notes |
|---|---|---|---|
| expense_categories | Hub | hub | replicated so tills can show names |
| expenses, expense_attachments, expense_recurring | back office (not replicated) | hub | like AP; terminal refuses writes |
| petty_cash_funds | Hub | hub | |
| petty_cash_entries | back office | hub | a till paid-out stays a `cash_event` (Append) and may *reference* an expense |
| cash_events.expense_id (new col) | Append | terminal | set only at creation |
| receipt_snapshots | Append | where the sale is committed | travels with the sale |
| sale_voids, sale_void_tenders | Append | terminal | |
| trading_day.cutoff (setting) | Hub | hub | |
| day_closes | back office | hub | close requires terminals' shifts closed and outboxes drained (or explicit "close with N unsynced" warning → after-close lines) |
| registers, cash_drawers | Hub | hub | |
| shifts.register_id/drawer_id | Shared (existing) | terminal | |
| cases, case_events | back office | hub | terminals see their own via pull later if needed |
| stock_lots | Hub | hub (receiving is hub-only already) | |
| stock_movements.lot_id | Append | hub for receive/waste/return; terminal sales carry NULL | FEFO consumption written by the hub as derived `lot_adjust`-free allocation table, see risk R3 |
| waste_records | back office | hub | movements it creates are Append |
| supplier_product_map ext. | back office | hub | |
| requisitions, PO approval fields, receipt_discrepancies, supplier_returns | back office | hub | |
| scale_barcode_rules, product_barcodes.kind | Hub | hub | tills must parse offline |
| product_merges | back office | hub | moved references replicate through their own tables |
| sales.channel, carts.channel | Append / local | terminal | |
| product_prices channel rows, pricing_policies | Hub | hub | |
| promotions, coupons, bundles | Hub | hub | evaluated offline on the till |
| alerts (cases) | back office | hub | |
| device_heartbeats ext. | existing | terminal → hub | |
| devices.credential_version | Hub | hub | |

A test in `tests/sync.rs` will assert every table in the schema is either in
`TABLES` or in an explicit `BACK_OFFICE` list — a new table without a decision
fails CI.

---

## G. Permission matrix (new)

| Permission | Owner | Manager | Accountant | Inventory | Cashier | Delivery |
|---|---|---|---|---|---|---|
| expenses.view | ✓ | ✓ | ✓ | | | |
| expenses.create (draft/submit) | ✓ | ✓ | ✓ | | | |
| expenses.approve | ✓ | ✓ | | | | |
| expenses.pay | ✓ | | ✓ | | | |
| petty_cash.manage | ✓ | ✓ | | | | |
| reports.profit (operating profit) | ✓ | ✓ | ✓ | | | |
| day.close (Z) | ✓ | ✓ | | | | |
| day.reopen | ✓ | | | | | |
| registers.manage | ✓ | | | | | |
| cases.manage (variance/alerts work) | ✓ | ✓ | | | | |
| pos.void_sale | ✓ | ✓ (cashier via bound approval) | | | | |
| lots.manage | ✓ | ✓ | | ✓ | | |
| waste.manage | ✓ | ✓ | | ✓ | | |
| replenishment.view (as built: `inventory.view`, `requisitions.create` or `purchasing.manage`) | ✓ | ✓ | | ✓ | | |
| requisitions.create | ✓ | ✓ | | ✓ | | |
| purchasing.approve (requisitions, POs, over-deliveries, invoice matches; as built, one permission) | ✓ | ✓ | | | | |
| supplier_returns.manage | ✓ | ✓ | | ✓ | | |
| catalog.merge | ✓ | | | | | |
| pricing.policy | ✓ | | | | | |
| promotions.manage | ✓ | ✓ | | | | |
| customers.statements | ✓ | ✓ | ✓ | | | |

All are added through `UPGRADE_PERMISSIONS` + `role_permission_seeds`, so an
owner who removed a permission from a role does not get it back.

---

## H. Migration strategy (no fabricated history)

- **Expenses / petty cash:** start empty. Historical `paid_out` cash events stay as they are; the expense report says "since <first expense date>". No conversion.
- **Business-date cutoff:** default 0 (= today's behaviour). Setting it later applies to new records only; stored dates are not recomputed.
- **Z close:** no Z for past days. The first Z is the first day the owner closes.
- **Registers:** migration creates one register and one drawer per existing device, named after it, and links only *open and future* shifts; closed shifts keep NULL ("before registers").
- **Receipt snapshots:** only new sales. Old receipts reprint as today and are marked "reconstructed" (no hash).
- **Sales channel:** existing sales stay NULL ("not recorded"); channel reports show that bucket explicitly.
- **Lots:** no lots for existing stock; it is "not lotted". Lots start at the next receiving where expiry is captured.
- **Stock-movement rebuild:** one `CREATE … _new / INSERT SELECT / rename` inside the migration transaction with row-count and Σ qty assertions; triggers recreated; backup taken before migrating (existing pre-upgrade backup path).
- **Supplier catalogue:** existing `supplier_product_map` rows become catalogue rows (they are already human-confirmed mappings); new columns NULL.
- **PO approval:** off by default; existing statuses valid.
- **Alerts:** `ai_alerts` rows copied into `cases` with their dismissed state; table kept read-only one release, then dropped.
- **Permissions:** seeded per G through the existing upgrade mechanism.

Each migration gets a test that runs it over a populated pre-migration fixture
and checks counts, sums and that nothing financial was created.

---

## I. Wave plan (with recommended adjustments)

The requested order is sound; repository evidence suggests five adjustments:

1. **Move business-date cutoff (B7) to the start of Wave 1.** Expenses, operating profit and statements all key on business date; doing the cutoff later would mean expenses written with one definition and sales with another.
2. **Do approval binding (Y42) inside Wave 1, before sale void (M6).** Sale void is the highest-risk new till action; shipping it on permission-only approval would need hardening later.
3. **One stock-movement schema rebuild at the start of Wave 3** covering waste, supplier_return, void, lot_id and business_date — rebuilding the largest append-only synced table once instead of three times.
4. **Generic `cases` table in Wave 2** (cash variance) and reuse it for the Alert Centre in Wave 7, instead of building variance cases and alerts separately.
5. **Sales channel (R32) early in Wave 5** before PLU/merge: channel pricing and promotions depend on it, and adding a nullable column is low risk.

Resulting sequence:

| Wave | Contents | Depends on |
|---|---|---|
| 1 Financial foundation | cutoff → receipt snapshots/hash → approval binding → sale void → expenses → petty cash → operating profit → credit ageing + statements | — |
| 2 Trading day & cash | X report → opening checklist → EOD control → Z close → registers/drawers → `cases` + variance investigation | 1 |
| 3 Inventory truth | movement rebuild → waste → lots → expiry capture/alerts → FEFO (advisory) → expiry/waste reports → days of cover | 1 |
| 4 Procurement | supplier catalogue → replenishment → requisitions → PO approval → receiving discrepancies → cost variance → 3-way match → supplier returns | 3 |
| 5 Retail-commercial | sales channel → channel pricing → pricing policy/rounding → margin protection → PLU → scale barcodes → duplicates → safe merge → address governorate/directions | 1, 4 |
| 6 Commercial | promotions → coupons → bundles | 5 |
| 7 Operational control | alert centre (on `cases`) → sync reconciliation → terminal health → credential rotation → `jobs` only if justified | 2 |
| 8 Intelligence | AI read tools for every new domain → AI proposals (drafts only) → document library → memory foundations → cash-flow radar | all |

Every wave: inspect → design note → invariants → migration (+ fixture test) →
backend → UI → permissions → EN/AR → tests (unit, domain, permission,
idempotency, migration, sync ownership, Arabic, failure) → full regression →
docs → one checkpoint commit with CI green on Linux and Windows.

---

## J. Risk register

| # | Risk | Area | Likelihood / impact | Mitigation |
|---|---|---|---|---|
| R1 | Sale void double-reverses stock or cash on retry, or voids a sale already refunded | financial, inventory | M / H | `sale_id UNIQUE` on `sale_voids`; same-day/no-refund/no-collection rules checked in the transaction; op-id idempotency; bound approval |
| R2 | Business-date cutoff applied inconsistently (reports vs commit) | financial | M / H | single function; reports read stored `business_date` columns only; test with 01:30 sales across cutoff |
| R3 | FEFO lot depletion diverges between terminals (terminals sell offline without lot knowledge) | inventory, sync | H / M | terminals never deplete lots; the hub derives FEFO allocation from synced sale movements in a recomputable table; counted lots override; UI labels "estimated by expiry order" |
| R4 | Stock-movement table rebuild loses or duplicates rows on a large DB | migration | L / H | pre-upgrade backup, in-transaction count + Σqty assertions, abort on mismatch, fixture with 100k movements |
| R5 | Late terminal records for a closed Z day | sync, financial | M / M | after-close adjustment lines on the next Z; close warns with the unsynced count per terminal |
| R6 | Expense paid from a till drawer counted twice (cash_event + petty cash) | cash | M / M | a till paid-out is only a `cash_event` with `expense_id`; petty fund entries only for the fund; report joins on one source |
| R7 | Channel pricing makes a till price differ from a delivery order price unexpectedly | commercial | M / M | channel fixed at cart creation; price shown with channel label; fallback to retail explicit |
| R8 | Product merge breaks historical reports | financial | L / H | sales/refunds keep snapshots + old product_id; only current references (barcodes, prices, mappings, open POs, lots, aliases) move; merge record kept; source archived not deleted |
| R9 | Scale-barcode rules ambiguous (two rules match) | pricing | M / H | rules validated at save to be prefix-disjoint; scan with >1 match refuses and asks |
| R10 | Promotions make cart pricing non-deterministic | pricing | M / H | promotions as an ordered stage in `price_cart` with explicit priority/stacking; golden tests: same input → same output |
| R11 | Approval binding breaks today's quick overrides | UX | M / M | binding only for high-risk list (void, Z reopen, large refund, price below cost); others unchanged |
| R12 | Too many alerts | UX | H / M | dedupe key per (kind, entity, day); severity ordering; Dashboard shows top items only |
| R13 | Replenishment suggests absurd quantities with little history | inventory | M / M | minimum-history rule; "not enough sales history" state; never auto-orders |
| R14 | Performance: days-of-cover / replenishment over 100k products | performance | M / M | SQL aggregates over indexed movements with a demand window; benchmark added to perf test (P95 < 2 s for full catalogue, single product < 50 ms) |
| R15 | Arabic statements/Z PDFs render wrong | UX | M / M | reuse the shaped-raster/PDF path; Arabic fixtures in tests |
| R16 | Scope: 50 items across 8 waves | delivery | H / H | each wave is a releasable checkpoint; no wave starts until the previous is green and documented |

---

## Not in this program (unless decided)

Multi-tenant, HR/attendance, production/BOM, marketplace settlement, loyalty
tiers, replacing the Rust WhatsApp adapter — as stated in the brief.

## Wave 5 delivered: retail commercial foundation (2026-10-06)

Design and rules: [PRICING_AND_CATALOGUE.md](PRICING_AND_CATALOGUE.md). It was delivered in seven
checkpoints, each committed separately:

1. barcode kinds, PLU and scale barcodes;
2. duplicate review and the product merge;
3. Bahrain address additions and the sales channel;
4. channel prices through the one resolver;
5. pricing policies, rounding and margin protection;
6. screens, Arabic, AI reads, reports and dashboard;
7. tests, performance and docs.

The sales channel came before channel pricing, as recommended in section I. Promotions, coupons,
bundles and kits stay in Wave 6.
