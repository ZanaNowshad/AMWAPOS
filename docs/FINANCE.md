# Money after the sale (Wave 1)

This document covers what happens to money after the till rings it up:
- the trading day;
- issued receipts;
- bound approvals;
- voids;
- expenses and petty cash;
- operating profit;
- customer statements.

All amounts are integer fils (`*_minor`). Financial history is never rewritten: a mistake is
reversed by a new record. The plan these follow is in [MERCHANT_OS_PLAN.md](MERCHANT_OS_PLAN.md).

## Trading day

Every sale, refund, expense and report belongs to a **business date**. It is the local date in
`business.timezone`, less the cutoff set in Settings → Shift → **Trading day ends at**. The
cutoff runs from midnight to 06:00 in 30-minute steps and defaults to midnight.

With a 02:00 cutoff, a sale at 01:30 on the 6th belongs to the 5th. Report ranges use the same
bounds (local midnight + cutoff), so "today" in reports matches "today" at the till.

Code: `time::day`, `time::business_date`, `time::local_date_range_utc`.

## Issued receipts

When a sale or refund commits, the exact receipt document is stored in `receipt_snapshots` in
the same transaction, with its SHA-256 (`receipt::digest`, format 1).
- Snapshots are append-only. Triggers refuse updates and deletes, and the table syncs to the hub.
- A reprint uses the snapshot, marked `*** COPY ***`, so it never picks up a later change to the
  product name, VAT number or footer.
- Admin → Sales → receipt preview shows the fingerprint.
- Records made before migration 0027 have no snapshot. They reprint as **reconstructed**
  (`exact: false`), and the preview says so. No history is fabricated.

## Bound approvals

A manager approval for a refund or a void is bound to that one request. The binding is
SHA-256 over:
- the action;
- the entity;
- the exact payload (lines, tenders, reason);
- the device.

The summary the manager sees (for example "Void receipt T01-000123 (BHD 1.250)") is kept on the
server, not sent by the client. The approval works once, for that exact request, within
120 seconds. If the cashier changes anything, a new approval is needed.

Code: `auth::binding`, `AppCore::authorize_bound`, and the `binding` argument of `auth.approve`.

## Voiding a sale

**Void** (till → More → Recent sales → a sale → **Void sale**) cancels a whole sale that was rung
up by mistake. It is allowed only when all of these hold:

| Rule | Otherwise |
| --- | --- |
| Same branch | refused |
| Not already voided | refused |
| Business date is today | use a refund |
| The shift that took it is still the open shift | use a refund |
| Nothing refunded from it yet | refund the rest |
| Not sent for delivery | use the delivery's not-delivered step, or a refund |

A cashier needs `pos.void_sale` or a bound manager approval. A reason is required.

What a void does:
- It writes a reversal through the same code path as a refund (`refunds.kind = 'void'`), so every
  net-sales, cash, VAT and stock figure stays correct without special cases.
- Every line is restocked (stock movement type `void`), and each tender is returned exactly as
  paid, including account credit and loyalty.
- It records one `sale_voids` row (unique per sale) and prints a "SALE VOIDED" receipt numbered
  `-V`.
- The original sale stays, listed as **Voided**. The refunds report shows voids separately.

## Expenses

Admin → Business → **Expenses** (`expenses.view`).

Lifecycle: **draft → submitted (waiting for approval) → approved (to pay) → paid**. A submitted
expense can be **rejected** (a note is required). A submitted, approved or paid expense can be
**voided**; it stays listed, and a paid one stops counting.

- **Entry:** pick a category (15 defaults, editable) and enter what it was for, the amount, and
  who was paid. VAT is entered only when the bill shows it, with `total = net + VAT`.
  Attachments are stored as originals under `data/expenses`.
- **Approval:**
  - Approved on entry when the person submitting may approve (`expenses.approve`), or when the
    total is within Settings → Expenses → auto-approve limit (default 0 = never).
  - Otherwise it waits for someone with `expenses.approve`.
- **Payment** (`expenses.pay`), by petty cash, till paid-out, bank transfer, card, cheque or
  other:
  - Petty cash writes a fund entry and refuses if the fund would go below zero.
  - A till paid-out links an existing paid-out cash event of the same amount. A paid-out backs
    one expense only, so cash is never counted twice.
- **Immutability:** once an expense is submitted, its money fields are frozen by a trigger. Only
  drafts can be deleted. A void writes a compensating petty-cash entry where needed and is
  idempotent.
- **Repeats** (rent, internet): a schedule makes **drafts** on their due dates, catching up at
  most 12. It never approves or pays anything.

## Petty cash

A fund has a running balance made only of immutable entries:
- open, top-up, reimburse;
- expense, and void (the reverse of an expense);
- adjust (needs a reason);
- count (records the difference between counted and expected).

A fund can be closed only at a zero balance. Permission: `petty_cash.manage`.

## Operating profit

Report **Operating profit** (`reports.profit`):

```
revenue        = (sales total − sales VAT) − (refunds total − refunds VAT)   [voids are refunds]
cost of goods  = cost of items sold − cost of items returned
gross profit   = revenue − cost of goods
operating exp. = approved and paid expenses by business date (accrual), excluding VAT
operating      = gross profit − operating expenses
```

It is **not net profit**: it leaves out depreciation, financing, owner drawings, tax and stock
write-offs. The report says so.

## Customer statements and ageing

Customer → **Account** tab (customer credit on) shows:
- the statement for a date range: opening balance, each charge, payment, refund and
  adjustment with its receipt number, and the closing balance;
- ageing.

Ageing buckets are current, 1–30, 31–60, 61–90 and 90+ days past due. The due date is the
business date plus the customer's **days to pay** (default 30, 0–365). Payments settle the oldest
charges first. Credit left over shows as negative current.

The statement downloads as a bilingual PDF, or is queued as a WhatsApp document (needs
`whatsapp.manage` and a phone number). The **Receivables** report lists every customer's ageing.

## Ownership and permissions

| Table | Where it lives |
| --- | --- |
| `receipt_snapshots`, `sale_voids` | Append; made on any till and synced to the hub |
| `expenses`, `expense_*`, `petty_cash_*` | Hub only; writes refuse on a terminal |

Every table is classified either in `sync::TABLES` or `sync::LOCAL_TABLES`. A test fails when a
new table has neither.

New permissions:
- `pos.void_sale`;
- `expenses.view`, `.create`, `.approve`, `.pay`;
- `petty_cash.manage`;
- `reports.profit`.

Defaults:
- The manager gets all of them except `expenses.pay`.
- The accountant gets view, create and pay, plus `reports.profit`.

Upgrades add these to existing roles, but never re-add a permission the owner removed.

## Migration 0027

- Takes the usual pre-upgrade backup.
- Rebuilds `stock_movements` once, adding the `void`, `waste` and `supplier_return` types and
  `lot_id`. A check aborts the upgrade unless the row count and the quantity sum are unchanged.
- Adds:
  - `refunds.kind`, with existing rows set to `refund`;
  - `customer_accounts.terms_days`, defaulting to 30;
  - the new tables above.
- Creates no snapshots, voids or expenses for the past.

## Tests

- `crates/amwapos-core/tests/wave1.rs`:
  - cutoff;
  - replication classification;
  - receipt immutability and reconstruction;
  - void rules, approval and account voids;
  - expense lifecycle and approval limit;
  - petty-cash balance;
  - paid-out linking and repeats;
  - statements and ageing.
- `tests/flow.rs`: bound refund approval.
- `tests/sync.rs`: snapshots reach the hub; expenses and petty cash are refused on a terminal.
- `e2e/wave1.spec.ts`: void at the till; expense to operating profit; statement and PDF.
