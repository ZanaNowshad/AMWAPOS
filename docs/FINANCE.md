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

# Closing the trading day (Wave 2)

## The model in one paragraph

Every sale, refund (including voids) and shift already carries the business
date it was made on, using the Wave 1 trading-day cutoff. A **Z close** is
the permanent record of one branch's trading day, made on the hub (or the
single computer of a one-till shop). It counts every record that no
earlier close has counted, and it lists exactly which ones in
`day_close_items`. A record is counted by one close only.

- **X (current totals)** is the same calculation, with nothing written.
- **Selling never depends on any of this.** A till keeps selling offline
  whether or not the day is closed.

## X: current totals

End of day → Today, or `day.x` (`day.x_report`). It shows:
- **Sales:** count, before discounts, discounts, sales, refunds, voids, net
  sales, VAT and net of VAT.
- **Payments and VAT:** tenders net of refunds, VAT by rate, and cash and
  non-cash sales.
- **Pay on delivery and customer accounts:** pay on delivery not collected
  at the till, and sales on account.
- **Drawers:** each drawer (open ones included) with float, cash sales and
  refunds, cash in and out, safe drops, delivery collections, expected,
  counted and difference.
- **Notes on the cash lines:** which cash out paid expenses, and which cash
  in were customer account payments. These are shown inside the existing
  lines, never counted again.
- **After-close records**, if any (see below).

X runs on a read connection only. The test `x_report_shows_the_day_and_changes_nothing`
compares the row count of every table, the change log and SQLite's
`total_changes()` before and after repeated X, checks, opening and the X PDF.
The figures reuse the existing definitions:
- net sales = sales − refunds − voids;
- refunds and voids reduce the day they were made;
- expected cash = `shifts::shift_summary`.

## Z: the close

End of day → **Close trading day** (`day.close`, permission `day.close`,
refused on a terminal).

- **Scope.** One branch and one business date. UNIQUE(branch, date): a day
  is closed once. Other branches are untouched; each has its own number
  `Z-<branch code>-00001`.
- **Order.** Dates close in order, from the branch's first close. These are
  refused:
  - a day before the last close;
  - a later day while an earlier one still has uncounted records (the
    message names the day to close first);
  - a future day.

  Records dated before the first close are not part of any close (the
  first close says so).
- **What is kept.** `snapshot_json` holds the full report and the rendered
  bilingual document, with its SHA-256. The report includes:
  - business name and VAT number, branch, date, cutoff and time zone;
  - totals, tenders, VAT by rate, drawers and cash;
  - after-close records;
  - who closed it and when.

  Viewing, the PDF and the AI read it from the stored record; nothing is
  rebuilt from today's settings. The drawer shows "Unchanged since it was
  closed" when the fingerprint still matches.
- **Immutable.** Triggers refuse updates and deletes on `day_closes` and
  `day_close_items`.
- **Exactly once.** The idempotency check, the checks, the record, its item
  list and the audit entry are one transaction.
  - A retry or double click with the same operation id returns the same
    close, even if the first response was lost.
  - The same id with another date is refused (`idempotency_mismatch`).
  - Another id for a closed day is refused.
  - A failure half-way leaves nothing behind (tested by injecting a failing
    trigger), and the retry closes once.

## Records that arrive after their day was closed

A till that was offline sends a sale of 5 October after 5 October was closed:
1. The sale is accepted and kept with its real time and business date. Sync
   is unchanged.
2. The close of 5 October is not touched.
3. X now shows it under **After-close adjustments**, with its original date,
   receipt and computer. The checks say how many there are.
4. The next close counts it once, flagged `late`, in a separate section, and
   prints a total for the close.

`day_close_items.late` and `business_date` record this, so any close can
be traced back to the records it counted. The same applies to refunds,
voids and shifts.

## Closing checks

`day.checks` classifies every item. Closing needs no blocking item; any
warning needs "I have read the items above".

| Level | Checks |
| --- | --- |
| Blocking | future day; day already closed; a later day is already closed; an earlier day with records is not closed; a shift still open on **this** computer |
| Warning | a shift open on another computer (its cash is counted in a later close); a terminal with records still to send or silent for 30 minutes (hub only); sync records that could not be saved; a drawer difference above the approval limit; cash differences still being looked into; riders holding cash; payment screenshots to check; backup overdue |
| Information | smaller drawer differences; expenses waiting for approval or payment; after-close records in this close; first close; "you can keep selling" when closing today |

Nothing is checked that AMWAPOS cannot know. For example, it cannot see
cash that was never recorded, or a terminal's work before it syncs.

## Opening the store

`day.opening` (End of day → Today, top card). It is advice and never blocks.
It checks:
- whether the last trading day with sales is closed;
- open medium or high cash cases;
- backup state;
- sync failures, or paused sync on a terminal;
- whether this computer is a register and has a drawer;
- whether a shift is open with a float;
- print jobs that failed in the last 24 hours.

The verdict is "Ready to trade" or "Ready, but these need attention".

## Registers, drawers and cash sessions

- **Register:** the checkout as a business thing ("Till 1"). It points at
  the computer that stands for it now. Choosing a computer moves that
  computer away from its previous register.
- **Drawer:** the cash a person is responsible for. A register has a default
  drawer, and can have more.
- **Cash session = the existing shift.** It is unchanged (float, events,
  expected, count, difference, approval) and now also records `register_id`
  and `drawer_id`.
- **Migration 0028:**
  - Creates one register and one drawer for each existing computer, with
    ids derived from the device id, so the hub and every terminal create
    the same rows. Inactive computers get an inactive register.
  - A computer added later gets its register from a trigger.
  - Past shifts keep `register_id`/`drawer_id` NULL: nobody recorded that,
    so it is not invented.

  The test `upgrading_keeps_every_shift_and_invents_no_register_history`
  builds a schema-27 store and upgrades it.
- Registers and drawers are hub-owned and copied to terminals. Changes need
  `registers.manage`. A register with an open shift cannot be switched off.

## Cash-difference cases

- **When a case opens.** A counted drawer that differs from expected by more
  than Settings → Shift → **Cash difference that opens a case** (default
  BHD 1.000) gets one case. A manager can also open one for any shift.
- **Where it is made.** Cases are made where they live: on the hub when the
  shift arrives by sync, or on a single computer at the shift close. Shifts
  closed before the upgrade are never swept.
- **Facts.** Fixed at opening:
  - shift, date, register, drawer, computer and cashier;
  - float, cash sales, refunds, in and out, safe drops, collections;
  - expected, counted and difference;
  - who accepted the count and the note at the count;
  - every cash movement of the shift.

  The title states the fact: "Drawer is BHD 6.250 short".
- **Steps.** new → seen → looking into it → resolved, or dismissed.
  - Notes, files (stored as originals) and the assignee can be added before
    the end.
  - Resolving or dismissing needs a note.
  - Resolving takes an outcome: counting mistake, cash found, wrong change,
    not explained, other.
- **Permanence.** Every step is a `case_events` row. Facts, history and
  finished cases cannot change (triggers). One case per shift (UNIQUE). A
  retried step changes nothing.
- **Permissions.** View with `cases.view`. Seen, start, note, assign and
  files need `cases.manage`. Resolve and dismiss need `cases.resolve`.

## Permissions and ownership (Wave 2)

| Permission | Owner | Manager | Accountant | Cashier |
| --- | --- | --- | --- | --- |
| `day.x_report` (X, checks, opening, closed days) | ✓ | ✓ | ✓ | |
| `day.close` | ✓ | ✓ | | |
| `registers.manage` | ✓ | ✓ | | |
| `cases.view` | ✓ | ✓ | ✓ | |
| `cases.manage`, `cases.resolve` | ✓ | ✓ | | |

Upgrades add these to built-in roles once and respect removals.

| Table | Where |
| --- | --- |
| `registers`, `cash_drawers` | Hub-owned, copied to terminals |
| `shifts.register_id`, `drawer_id` | With the shift (shared) |
| `day_closes`, `day_close_items`, `cases`, `case_events` | Hub only; terminals refuse to close or act on cases |

The AI may read X, checks, closes, opening, cases and registers. It cannot
close or reopen a day, act on a case, change a count or change a register:
`NO_TOOL` lists each, and `ai_actions` classes them as financial commits.

## Tests (Wave 2)

- `tests/wave2.rs` (14):
  - X changes nothing;
  - Z once, with retry and mismatch;
  - Z independent of later settings and immutable;
  - late sale counted once in the next close;
  - days close in order;
  - the cutoff boundary uses the stored date;
  - branches close separately;
  - expected cash through sales, refunds, voids, cash in and out, safe
    drops, an expense paid out of the till, and a pay-on-delivery
    collection;
  - registers and moving them;
  - upgrade from schema 27;
  - case lifecycle and permanence;
  - permissions;
  - failure half-way and retry;
  - opening.
- `tests/sync.rs`: a till offline during the close sends its sales into the
  next close, and the hub opens its variance case.
- `e2e/wave2.spec.ts`: a short drawer becomes a case; the day is closed once.

## Wave 8: Cash-flow Radar and finance evidence

Rules and formulas: [INTELLIGENCE_AND_EVIDENCE.md](INTELLIGENCE_AND_EVIDENCE.md) §7.
- **Cash-flow Radar** (`cashflow.view`; owner, manager, accountant) for the next 7–90 days, in
  integer fils:
  - **Known:** posted AP on its due dates and approved, unpaid expenses. Anything due earlier
    counts today, marked overdue. Unmatched payments and credits are shown apart.
  - **Scheduled:** repeating expenses, counted once even after their draft is made.
  - **Exposure:** expenses awaiting approval (and the AP items in PROCUREMENT.md).
  - **Scenario:** sales less refunds over 28 days, only with that much history.
  - Also shown: cash recorded in drawers, petty cash and with riders; what customers owe by
    age, with no date.
  - It is never presented as a bank balance.
- **Finance documents** (bank, tax and statement categories) need a finance permission on top of
  `documents.view`. An expense receipt needs `expenses.view`; a supplier invoice needs
  `payables.view`.
- **Expense attachments** (Wave 1) appear in the Document Library, linked to their expense, with
  no copy of the file and their original author and time. They are now carried by backups.
- **Assistant:** `propose_expense_draft` creates a draft only, and a person submits it. Expense
  entry alone does not open proposals (the accountant stays read-only for the assistant).
