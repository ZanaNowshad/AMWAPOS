# Architecture

## Processes and layers

```
React UI ──invoke("rpc",{cmd,token,args})──▶ Tauri shell ──▶ amwapos-hub::Runtime ──▶ amwapos-core::commands::dispatch ──▶ AppCore
                                                                  │                                                    │
                                                                  ├─ hub HTTP server :47800 (hub mode)                 └─ SQLite (WAL, FULL sync)
                                                                  ├─ UDP discovery :47801
                                                                  ├─ terminal sync loop (terminal mode)
                                                                  └─ scheduled backup loop
```

The UI holds no business rules it relies on. It sends commands and renders what comes back.
Prices, tax, discounts, totals, stock, permissions and approvals are all decided in Rust. The
same `dispatch` serves Tauri IPC, the devserver (`/rpc`) and the tests.

## Storage

- A single SQLite database, `amwapos.db`, in the data directory
  (`%ProgramData%\AMWAPOS\data`; override with `AMWAPOS_DATA_DIR`).
- `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`. One writer connection (every write is
  `BEGIN IMMEDIATE`) plus a pool of readers.
- Migrations are embedded, numbered and SHA-256 checksummed. The app refuses a database whose
  applied migration was altered, or one newer than itself. Before migrating an existing database
  it takes a safety backup with `VACUUM INTO`.
- A `store.marker` file stops the app from ever silently recreating an empty database over a
  missing one.
- Financial history is append-only. Triggers reject `UPDATE`/`DELETE` on sales, sale items,
  payments, refunds, cash events, stock movements and audit logs. Corrections are new rows.

## Money

- All amounts are `i64` minor units (fils), quantities are `i64` thousandths, rates are basis
  points. Division rounds half away from zero.
- A cart discount is spread across lines by largest remainder, so line amounts always add up to
  the total exactly.
- VAT can be inclusive or exclusive per product tax rule.
- A refund is prorated from the original line. The final refund of a line takes the exact
  remainder, so refunds can never exceed what was paid.
- The UI parses decimal strings digit by digit. `parseFloat` is banned by lint.

## Exactly-once operations

Every mutation that must not happen twice (finalize a sale, refund, cash event, bulk price
change, import, stock operations) carries a client `operation_id`. The idempotency check, the
mutation and the stored result commit in one transaction. A retry returns the stored result.
The same id with a different payload fails with `idempotency_mismatch`.

## Auth

- Staff sign in with a PIN, hashed with Argon2id (19 MiB, 2 iterations). Repeated failures lock
  the account.
- Sessions are random 256-bit tokens kept only in memory. They lock after idle time and expire
  after 16 hours.
- Permissions belong to roles and are enforced by `AppCore::authorize` on every command.
- A manager approval produces a single-use token, valid for 120 seconds and bound to one
  permission. The audit log records both the cashier and the approver.
- The audit log is a SHA-256 hash chain. Admin → Audit verifies it.

## Sync (multi-terminal)

- **Roles:** one hub (the back-office PC) and N terminals. A standalone store can be turned into a
  hub; a terminal joins with the hub address and an 8-digit single-use pairing code (15-minute
  TTL, rate limited).
- **Capture:** triggers write every change to `sync_outbox` (table, primary key, operation,
  origin). Rows applied from the hub are not re-captured.
- **Transport:** requests are HMAC-SHA256 signed with a per-device key derived from the hub master
  secret. Each request has a nonce and a timestamp within ±5 minutes, and responses are signed
  too. The master secret lives in Windows Credential Manager. The database holds only its
  fingerprint, so a lost credential is reported instead of being silently replaced.
- **Policies:**
  - Hub-owned: the catalogue, prices, users and settings are edited only on the hub.
  - Append-only: sales, refunds, cash and stock movements are appended, and the hub checks that the
    device owns each row it pushes.
  - Shared: customers use last writer wins.
- **Stock:** the hub replays stock movements to recompute balances.
- **Cycle:** a terminal pushes, then pulls (excluding its own origin). Changes that fail to apply
  go to a dead-letter queue, which can be retried from Admin → Sync.
- **Safety:**
  - A terminal records the hub's `hub_instance_id` and refuses a hub that was rebuilt or rolled
    back, until a person decides.
  - A schema version mismatch refuses pairing.

## Printing

After the sale commits, a print job is queued in the same transaction, and printing starts once
the commit is done. A printer failure never undoes a sale; the receipt can be reprinted.
Backends:
- ESC/POS over TCP
- Windows spooler (raw, via winspool)
- file
- none

## Frontend

The UI has two modes:
- **Cashier Mode:** full-screen checkout with no admin navigation. The scanner input is always
  focused. F6/F7/F8 open payment, and Enter confirms.
- **Admin Mode:** a sidebar with every back-office section. Items are hidden when the user lacks
  the permission, and the backend enforces permissions anyway.

The design tokens are in `src/styles/tokens.css`.
