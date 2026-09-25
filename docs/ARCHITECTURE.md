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
- **Transport (sync protocol 2):**
  - **Pairing:** SPAKE2 over the 8-digit pairing code. Both sides use the SHA-256 of the code as
    the password, which is what the hub stores. The terminal's pairing request and the hub's
    reply (device key + snapshot) are sealed with keys derived from the SPAKE2 secret.
  - **After pairing:** each body is sealed with ChaCha20-Poly1305 under direction-specific keys,
    derived with HKDF-SHA256 from the per-device key. The associated data binds method, path,
    device, timestamp and nonce. Requests and replies are also HMAC-signed over the ciphertext,
    with a nonce cache and a ±5 minute clock window.
  - **Keys:** the per-device key is derived from the hub master secret, which lives in Windows
    Credential Manager. The database holds only its fingerprint, so a lost credential is
    reported instead of being silently replaced.
  - **Versioning:** the hub refuses any other protocol version with HTTP 426. `/info` reports the
    version, and terminals check it before pairing and before every sync cycle. Code in
    `crates/amwapos-core/src/channel.rs`.
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

## Receipts and Arabic

Receipts are built only from committed records (sale lines snapshot the product name, and its
Arabic name since schema 3). ASCII lines print in the printer's text mode. Any line with Arabic
or other non-ASCII text is:

1. ordered with the Unicode bidi algorithm (`unicode-bidi`);
2. shaped with full OpenType Arabic shaping (`rustybuzz`), including joining forms and lam-alef;
3. drawn with embedded, subset Noto Sans Arabic / Noto Sans (SIL OFL);
4. sent as an ESC/POS `GS v 0` raster image at 12 dots per character column (576 dots on 80 mm).

This works on any ESC/POS printer, with no Arabic code page needed (`crates/amwapos-core/src/raster.rs`).

## UI language

- **Keys:** English source strings are the translation keys: `t("Close shift")`,
  `t("Shift {0}", n)`.
- **Dictionary:** `src/i18n/ar.ts` holds the Arabic strings.
- **Backend text:** error messages, report labels and diagnostics go through `tb()`. It tries an
  exact match first, then patterns derived from the Rust `format!` strings. Values inside those
  patterns are translated too, and English dates are localised.
- **Codes:** status and type codes use `codeLabel()`.
- **Direction:** Arabic sets `dir="rtl"` on the document. CSS uses logical properties.
  Directional icons are mirrored. Money is wrapped in Unicode isolates so it keeps its
  left-to-right order.
- **Tests:** a unit test fails when any `t()` key or status label has no Arabic entry.

## Frontend

The UI has two modes:
- **Cashier Mode:** full-screen checkout with no admin navigation. The scanner input is always
  focused. F6/F7/F8 open payment, and Enter confirms.
- **Admin Mode:** a sidebar with every back-office section. Items are hidden when the user lacks
  the permission, and the backend enforces permissions anyway.

The design tokens are in `src/styles/tokens.css`.

## WhatsApp and OCR (optional modules)

Both are off by default (flags `whatsapp.enabled`, `whatsapp.send_receipts`,
`whatsapp.delivery_notices`, `ocr.enabled`, `ocr.payment_screenshots`, `ocr.supplier_invoices`;
a sub-flag counts only when its parent is on). Neither uses a second process for WhatsApp or a
local network port.

```
React ── Tauri commands only (status, start/stop, pair code, queue, mark read, recent) ──┐
                                                                                        │
Runtime ── WhatsAppService (crates/amwapos-hub/src/whatsapp/service.rs)                 │
             ├─ supervisor task: owns the client, restarts it with back-off             │
             ├─ I/O worker task: outbox sends, read receipts, media downloads           │
             └─ WhatsAppAdapter trait                                                   │
                  ├─ RustWhatsAppAdapter  (whatsapp-rust =0.7.0, unofficial Web client)  │
                  └─ FakeAdapter          (tests)                                        │
         ── OcrWorker (crates/amwapos-hub/src/ocr_worker.rs): bundled Tesseract,        │
            one child process per image, on its own task                                │
AppCore ── wa_outbox / wa_inbox / payment_reviews / invoice_scans (ledger DB) ◄──────────┘
```

How the rules are enforced:

- **Separate session store.** `SessionPath::for_data_dir` is the only way to get the session
  location: `<data>/whatsapp/session.db` (`%ProgramData%\AMWAPOS\data\whatsapp` on Windows). It
  refuses a relative path, the ledger file (`amwapos.db`) and the executable's folder. The file is
  opened only by the crate's own SQLite store inside the adapter; AMWAPOS' connection pool never
  opens it. The explicit owner-only session backup opens its own read-only connection.
- **No lock across the client.** The supervisor keeps the client's exit future in its own task
  state and receives commands over a channel; status is published through a `watch` channel.
  A client that ends or panics is restarted with back-off (2 s doubling to 5 min) unless it was
  stopped or logged out. The runtime's minute watchdog re-spawns a supervisor or worker task that
  died. Release builds use `panic = "unwind"`, so a panic stays in its task.
- **Selling never waits for WhatsApp.** Sales, deliveries and payment confirmations only insert
  outbox rows after they commit (`wa_after_sale`, `wa_after_delivery`,
  `wa_after_payment_confirmed`); a failure there is logged, never returned. Sending happens in
  the I/O worker.
- **Persist first.** Inbound messages are committed to `wa_inbox` in the crate's inbound
  durability hook, before WhatsApp is acknowledged and before any status or UI update. Media is
  recorded as `pending` and downloaded afterwards; images open a payment review.
- **Idempotent sends.** `whatsapp.queue` stores a payload hash with the operation id: the same key
  and payload return the first row; a different payload fails with `idempotency_mismatch`. The
  WhatsApp message id is derived from the outbox id, so a retried send is the same message.
- **OCR is assistance.** Payment screenshots end as `ocr_match | likely_match | mismatch |
  needs_review`; only a person confirms. Invoices become a draft purchase order. Models are
  checked against `ocr/models/models.json`; without the English model OCR reports
  `ocr_model_missing` and cannot be switched on.
