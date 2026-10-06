# Completion status

**Overall: feature-complete for the Windows soak, not release-complete.** A row is **Complete**
only when it is implemented, tested automatically in this repository, and needs no Windows
hardware or third-party account. Everything that needs the store's hardware stays **Partially
complete** until the owner signs off the hardware pass in [OPERATIONS.md](OPERATIONS.md).

Classes:
- **Complete:** implemented and tested here; no hardware or vendor dependency.
- **Partially complete:** implemented and tested off-target; hardware verification is pending.
- **Blocked:** needs something outside the repository before work can continue.
- **Deferred:** deliberately not built yet. The admin page says **Not enabled**. Each needs an
  owner, a key and a rollback plan.

## Soak installer (CI)

| | |
| --- | --- |
| File | `AMWAPOS_0.1.0_x64-setup.exe` (artifact `amwapos-windows-unsigned`) |
| SHA-256 | `d6c0e260be719cd05a3755601bc7c70ea95153e535695173934ab371ee94f452` |
| Built by | GitHub Actions CI run #12 (`36024872450`) on `windows-2022`, commit `ad7e650` |
| WebView2 | Bootstrapper **embedded** (`webviewInstallMode: embedBootstrapper`); the CI job fails if the configuration changes to an install-time download |
| Signing | **Unsigned.** For internal soak only (SmartScreen will warn). |
| Download | https://github.com/ZanaNowshad/AMWAPOS/actions/runs/36024872450 (artifact `amwapos-windows-unsigned`, id 10819612747, kept for 90 days) |

Evidence for that installer, re-run on 2026-09-24 (the latest evidence is in "Platform pass, 2026-10-03" at the end):
- **Rust** (`cargo test --workspace`, Linux and Windows CI), 77 tests:
  - 38 core unit tests;
  - 7 back-office, 15 flow, 6 printing and 6 sync tests;
  - 4 encrypted-channel tests and 1 HTTP sync test.
- **Lint:** `cargo fmt` and `clippy -D warnings` are clean; `tsc` and `eslint` are clean.
- **Frontend unit tests** (vitest): 20.
- **End-to-end** (Playwright): 3 flows.
  - Owner: setup → sale → split pay → refund → shift close, with the backup banner.
  - Cashier: Admin blocked, manager PIN, wrong PIN, audit row, and a scanner burst.
  - Arabic: right-to-left sale, admin, dark/compact theme.
- **Proxy test:** no product name, barcode, receipt number, pairing code, device key or PIN hash
  crosses the LAN in clear.
- **Performance** (100k products, release build, Linux sandbox): P95 scan 0.73 ms, search
  23.6 ms, cart 0.78 ms, sale commit 9.9 ms.

## Known limits

| # | Limit (frozen wording) | State |
| --- | --- | --- |
| 1 | Arabic receipt glyphs print as `?` (image-fallback not built). | **Fixed in code** (commit `eda19ef`). Arabic lines are shaped and rasterized (`GS v 0`), and tests prove no `?` reaches the printer. Still open until Arabic is seen on the store's 80 mm printer (hardware pass). |
| 2 | No RTL shell. | **Fixed** (commit `39652d4`). Cashier and admin work fully in Arabic right-to-left. |
| 3 | DB file unencrypted (BitLocker-dependent). | **Open.** See SECURITY.md: stolen-database paragraph and residual risks. |
| 4 | Unsigned installer; not run on Windows. | **Open.** The installer is now built and tested by Windows CI (above), but it is unsigned and has not been installed on a store Windows 10/11 machine. |

## Matrix

| Area | Status | Evidence | Pending |
| --- | --- | --- | --- |
| Integer money, VAT incl./excl., discount allocation, rounding (backend and UI previews) | Complete | `money`, `pricing` tests; vitest `mulDivRound` (half away from zero, as backend) | — |
| Server-authoritative cart & pricing, effective-dated prices | Complete | Flow and back-office tests. Price lookups use the application clock (a Windows CI clock-skew bug was fixed). | — |
| Exactly-once sale / refund / cash / bulk price / import | Complete | Idempotency replay and mismatch tests; lost-response sync test | — |
| Append-only financial history, hash-chained audit | Complete | Trigger tests; audit verify; E2E "Audit chain verified" | — |
| PIN auth (Argon2id), lockout, roles, backend permission checks | Complete | Core tests; E2E cashier blocked from Admin | — |
| Manager override (single-use, permission-bound, audited) | Complete | Core tests; E2E: wrong PIN changes nothing; audit row names the approver | — |
| Shifts, blind close, variance approval, cash in/out | Complete | Flow tests; E2E expected-cash check | — |
| Refunds bounded by refundable qty, exact proration | Complete | Flow tests; E2E refund | — |
| Inventory ledger, weighted-average cost, adjustments, stocktake | Complete | Back-office tests | — |
| Suppliers, purchase orders, receiving | Complete | Back-office tests | — |
| Customers, addresses, deliveries, delivery board (events, payment state) | Complete | Core tests; board with translated columns | — |
| Reports (14) + CSV export (formula-injection safe) | Complete | Report tests; CSV escape round-trip test | — |
| CSV product import | Complete | Preview/apply tests, duplicate isolation, scientific-notation guard, 100k import | — |
| Backup / verified restore / safety backup | Partially complete | Round-trip and tamper tests; E2E backup | USB / network-share folder checked in the soak (OPERATIONS checklist) |
| Backup-while-closed | Complete (operational rule) | OPERATIONS "Backup rule" (the hub keeps AMWAPOS running). Red banner on every Admin page and a till header pill, with one-click Backup Now. `backup.health` reports ok / overdue / failed. | A Windows scheduled task was deliberately not built (reasons in OPERATIONS) |
| Diagnostics | Complete | Readable details; last backup shown in local time with age; sync errors classified | — |
| Sync protocol v2 (encrypted, SPAKE2 pairing, one live code, burn after 5, pairing-id reuse rejected, versioned) | Complete | Encrypted-channel tests: proxy test, protocol 1 refused, burn, version mismatch shown as "Update needed", not offline | — |
| Lost hub credential | Complete | Reported, never silently replaced; owner reset + re-pair (sync test) | — |
| Multi-terminal operation on store Wi-Fi | Partially complete | Sync convergence tests; signed + encrypted HTTP tests on loopback | Two tills + hub on store Wi-Fi; unplug the hub mid-sale |
| Printing: receipt, COPY reprint, cash-drawer pulse, failure keeps sale | Partially complete | `tests/printing.rs`: receipt content and width; COPY; drawer pulses on cash only (not card, not reprint, not when off); failed printer keeps the sale and retry works; ESC/POS framing | 80 mm printer + drawer (network and Windows spooler) |
| Arabic receipts | Partially complete | Raster unit tests (shaping, lam-alef, bidi, placement); sale/refund receipts with Arabic names and bilingual labels; no `?` in text mode; test page has an Arabic line | Arabic on the physical printer |
| Barcode scanner (HID wedge) | Partially complete | Vitest heuristics; E2E burst of 5 scans at scanner speed, none dropped, field cleared | Physical USB scanner |
| Mid-sale power loss | Partially complete | WAL + `synchronous=FULL`; sale commit is one transaction; exactly-once replay tests | Pull the plug mid-sale on a till |
| Cashier Mode / Admin Mode UI, English and Arabic (RTL), light/dark, density | Partially complete | Playwright English + Arabic flows, dark/compact screenshot. The unit test fails on any untranslated `t()` key or status label. Backend messages translated through `tb()`. `e2e/layout1024.spec.ts` drives 10 states at 1024×768 (and PAY/Confirm at 1024×700) and measures PAY (≥200×56, on screen, nothing on top), the 56/88 px chrome, 48 px touch targets and no horizontal scroll; see docs/UI.md. | Visual and touch check on the 1024×768 till panel (WebView2) during the soak |
| Performance targets (100k products) | Partially complete | Numbers above (Linux sandbox) | Re-measure on the till hardware during the soak |
| Windows desktop shell (single instance, Credential Manager, ProgramData ACLs, 30-day rotating logs) | Partially complete | Built and unit-tested on Windows CI; launched under Xvfb on Linux | First launch on a Windows 10/11 till |
| NSIS installer (per-machine, firewall rules, ACLs, data kept on uninstall, embedded WebView2) | Partially complete | Built by Windows CI (above); embedded WebView2 enforced by CI | Install / upgrade / uninstall on Windows 10 and 11 |
| CI (lint, types, unit, E2E, Windows build + installer) | Complete | GitHub Actions green on this branch | — |
| Release workflow (tag → draft release, SBOMs, SHA-256 sums, optional signing) | Partially complete | Workflow lint-clean; SBOM generation run locally | First tag run |
| Code signing | Deferred | Unsigned is accepted for internal soak | Authenticode certificate |
| Auto-update (flag `updates`) | Partially complete | Ed25519-signed manifest, size + SHA-256 checks, re-verify before install, safety backup; refuses unsigned builds (`tests/updates.rs`) | Signing key (`AMWAPOS_UPDATE_PUBKEY`), hosting, install run on Windows |
| WhatsApp (flags `whatsapp.enabled`, `.send_receipts`, `.delivery_notices`) | Partially complete | In-process `whatsapp-rust` =0.7.0 adapter behind a trait; supervisor restart after panic, separate status flags, reconnect after restart, idempotent sends, persist-before-ack inbound, media → review (`tests/whatsapp.rs` with `FakeAdapter`); post-commit receipts/notices, payload-hash idempotency, EN/AR templates (`tests/automation.rs`) | Pairing and soak test with a real phone; the real adapter has never connected to WhatsApp |
| OCR (flags `ocr.enabled`, `.payment_screenshots`, `.supplier_invoices`) | Partially complete | Separate worker running bundled Tesseract with SHA-256-checked eng+ara models; `ocr_model_missing` keeps it off; statuses ocr_match/likely_match/mismatch/needs_review; draft PO only (`tests/ocr.rs` with real Tesseract, `tests/automation.rs`) | Accuracy on real supplier invoices and BenefitPay screenshots |
| AI assistant (flags `ai.enabled`, `ai.mutations`, `ai.dual_control`) | Partially complete | Read and proposal tools as the signed-in user; confirm runs the normal command; undo by compensating record where one exists; fake-provider and loopback-stub tests (`tests/ai*.rs`) | Vendor API key and owner consent; never called against a real provider or OpenRouter here |
| Card terminal / BenefitPay integration | Deferred | Tenders recorded manually with a reference | Provider SDK, merchant account, owner, rollback |
| Migration (CSV/XLSX/ZIP/folder) | Complete | Detect → map → preview (no writes) → apply through normal commands (`tests/migration.rs`) | — |
| Customer credit (flag `customer_credit`) | Complete | Append-only ledger, limit + manager override, refunds, cash payments in drawer (`tests/credit.rs`) | — |
| Windows Hello step-up (flag `windows_hello`) | Partially complete | Runtime gate + audit (`tests/step_up.rs`); WinRT call type-checked for Windows | Run on a Windows machine with Hello |
| PDF receipts (flag `pdf_receipts`) | Complete | Raster PDF after commit; failure never affects the sale (`tests/automation.rs`) | — |
| Database encryption at rest | Deferred (known limit 3) | BitLocker guidance and threat paragraph in SECURITY.md | Owner decision |


## Spec pass (items 3–42), 2026-09-25

Flags (all default off): `hub`, `whatsapp.enabled`, `whatsapp.send_receipts`, `whatsapp.delivery_notices`,
`ocr.enabled`, `ocr.payment_screenshots`, `ocr.supplier_invoices`, `ocr.ai_parse`, `ai.enabled`, `ai.mutations`,
`customers.credit`, `windows_hello`, `pdf_receipts`, `updates`. Old keys (`whatsapp`, `ocr`, `payment_reviews`, `ai`,
`ai_mutations`, `customer_credit`) are read as aliases. Store settings: negative stock allowed with a warning (default),
costing method `weighted_average` (receiving updates cost, audited) or `manual`, receipts 80 mm (58 mm option), EN or EN+AR labels.

Not in this pass by decision: live WhatsApp link and real photos (owner soak), code signing, card SDK, Cloud API,
hub TLS rewrite, Task Scheduler backups, the full acceptance matrix. The updater is AMWAPOS' own Ed25519-verified
flow rather than tauri-plugin-updater; it refuses unsigned builds and opens the installer window (no silent apply).

## Product brief: 12 pillars, 2026-09-25

Every new module is behind a flag that is off by default. With all flags off,
the new tables exist (migrations 0009, 0010) but nothing reads or writes them.
`customers.credit` was not touched and stays off.

| Flag | Module |
|---|---|
| `inventory.locations` | Stock locations; transfers draft → ship → receive (idempotent op ids, in-transit qty, no stock creation) |
| `loyalty.enabled` | Integer points ledger; earn on committed sales; redeem as a discount priced by `price_cart`; proportional reversal on refund |
| `orders.digital` | Phone/WhatsApp/web/other orders; human confirm; idempotent load into a till sale; optional delivery on commit |
| `org.multi_branch` | Branch CRUD, user branch assignment, session branch switch, branch prices, branch pairing, branch-scoped mutations and reports |
| `pwa.companion` | Hub-served read-only owner phone page; hashed, revocable bearer token (≤ 24 h), LAN peers only |

Always on (no flag, no behaviour change for existing flows): ticket number on hold
and recall by number, low-stock hint on cart lines, safe-drop running total and
expected-cash formula on the shift screens, supplier last cost on the product,
supplier performance report, saved report date ranges, end-of-day pack + CSV zip.

Limits: one hub per LAN holds all branches (no cross-hub mesh). The phone page's
offline shell needs HTTPS for its service worker; on plain LAN HTTP the page
still shows the last snapshot it received. Soak, signing keys, the live WhatsApp
phone and Windows Hello hardware remain the owner's checks; nothing here is
claimed as soak-tested or production-ready.

Tests: `crates/amwapos-core/tests/pillars.rs` (transfers in transit + idempotency,
loyalty money/VAT/refund reversal, digital-order idempotent convert, multi-branch
isolation with the flag on and identical behaviour with it off, end-of-day pack)
and `crates/amwapos-hub/tests/companion.rs` (flag, token, revoke, LAN routes).

## AI: bring your own API key, 2026-09-26

- Providers: `fake | openai | anthropic | google | openrouter | custom`. The fake
  model is active whenever provider=fake or no key is stored; it never uses the
  network. No OAuth / device-code / subscription sign-in exists or is planned.
- Keys and the optional extra-header value live only in Windows Credential
  Manager (service `AMWAPOS`, accounts `ai/<provider>` and `ai/<provider>/header`).
  Settings (provider, model_id, base_url, header name, max_output_tokens,
  timeout_ms, model list cache) are in SQLite; no secret is.
- Owner-only: `ai.configure`, `ai.test`, `ai.models`. Changes apply on the next
  request (settings are re-read every round).
- Every prompt starts with `crates/amwapos-core/src/ai_prompts/constitution.txt`
  (verbatim) plus matching lines of `ai_prompts/playbooks.txt`; untrusted text is
  wrapped in `<<<DATA … END DATA>>>`; proposals need the user's own change request,
  and zeroing stock after DATA was read is refused. One retry on 429/502/503.
- Errors: `AI_NOT_ENABLED`, `AI_NO_KEY`, `AI_PROVIDER_ERROR`, `AI_TIMEOUT`,
  `AI_MODEL_NOT_FOUND`. Diagnostics export replaces any stored AI secret value.
- Tests: `crates/amwapos-hub/tests/ai_byok.rs` (loopback stubs only).

## AI: full admin tool map, 2026-09-26

Status: **Partially complete.** Tested here with the offline test model only;
never run against a live provider, the live WhatsApp phone or Windows Hello.

- `crates/amwapos-core/src/ai_tools.rs`: 71 read tools and 93 `propose_*` tools,
  each an existing command. A tool is offered only with `admin.access`, one of its
  permissions, the owner role when owner-only, and its feature flag. The
  accountant role never gets proposal tools. With `ai.mutations` off, every
  `propose_*` tool is hidden and refused. Lists are capped at 50 rows.
- Every other command is listed in `NO_TOOL` with its reason (forbidden, till or
  setup only, file/binary, or covered by another tool). A test fails when a new
  command has neither a tool nor a reason.
- Writes only record a proposal (`command:<cmd>`, migration 0011). Confirm runs
  the same command through `Runtime::dispatch` with the confirmer's session, so
  permissions, owner-only rules, manager approval and Windows Hello step-up all
  apply. A failed human check (wrong PIN, cancelled Hello) leaves the proposal
  open. Command proposals are irreversible from the AI page (no fake undo).
- PINs and approval tokens come only from the Confirm card and are never stored
  or sent to the model. One-time secrets (phone-view link) are returned once to
  the card and stripped from the stored result. QR, pairing codes, PIN hashes,
  keys and session data are stripped from every read.
- Limits: 30 proposals per hour per user, 200 items per bulk proposal, proposals
  expire after 60 minutes, optional daily token cap (Settings → AI). After DATA is
  read in a conversation, every proposal is high risk.
- A reply with figures and no tool call gets one "[AMWAPOS check]" nudge; if it
  still has no tool call it is shown as **Unverified**. Answers carry evidence
  chips; record paths become links; a barcode in the question names its product.
- AI page: action inbox with today's digest, playbook buttons (eod, cash_short,
  reorder, refund_spike; reads only, no model), before/after diff on each card.
  Invoice scan: **Improve parse** (`invoicescan.ai_parse`).
- Consent is per provider: moving between two real providers asks the owner to
  agree again.
- Tests: `crates/amwapos-hub/tests/ai_admin.rs` (15), `ai_byok.rs` unchanged.

## AI workspace, 2026-09-26

Status: **Partially complete.** Tested with the offline test model and loopback
provider stubs (Anthropic and OpenAI-style SSE); never run against a live
provider, OpenRouter, the live WhatsApp phone or a Windows till.

- **C8 streaming and transparency.** With a `stream_id`, every provider streams
  (Anthropic SSE with `thinking.display: "summarized"`, thinking signatures
  replayed unchanged; OpenAI-compatible SSE with reasoning and tool-call deltas;
  Gemini SSE with thought summaries). The page polls `ai.stream` and shows, as
  they happen: thinking, each tool call with its input, each tool result exactly
  as the model saw it, nudges, fallbacks, token use. Stored conversations keep the
  same trace (thinking, calls with results).
- **C4 free fallback.** Owner opt-in, with its own consent and an OpenRouter key in
  Credential Manager. Used only when the chosen provider is unavailable (timeout,
  unreachable, 429, 5xx, unknown model), never for a wrong key or a refusal.
  Default model `openrouter/free` (editable). Shown on the page and audited
  (`ai.fallback`).
- **A5 photos.** Attach a photo for the model (PNG/JPEG/WEBP/GIF, 5 MB, content
  sniffed, owner-only), or send a supplier invoice / payment screenshot into the
  existing OCR pipelines from the same button. A photo marks the thread untrusted.
- **A6 memory.** Rename conversations; pin up to 8 records (product, customer,
  supplier, order, shift, PO, sale, delivery), read with the user's permissions,
  labels passed as DATA.
- **A8 briefings.** A playbook at a time of day and days of the week, run while
  the app is open with the permissions of whoever saved it (short internal
  session, ended after the run), once per day; optional AI summary with a real
  provider. Notes are listed on the AI page (`ai.notes`). Audited.
- **E1 triage.** Incoming WhatsApp messages sorted into order / payment /
  complaint / question / spam / other by rules, optionally re-sorted by the AI;
  a person can correct each one. Suggested next step per category.
- **E2 draft reply.** AI draft (or a template when no provider) put in the reply
  box; nothing is queued or sent until a person presses Send. Audited.
- **E4 payment comparison.** Every payment review carries expected vs detected,
  the difference, and reference / confidence / duplicate checks. It never settles.
- **F1 slash commands.** 46 commands with a palette: direct reads (no model),
  playbooks, pin/rename/new, `/price`, `/explain`, `/goto`, `/attach`, `/model`.
- **F5 keyboard.** Ctrl+K, `/`, `?`, Alt+N/I/B, Enter/Shift+Enter, ↑ recall, Tab
  complete, Esc; Ctrl+Enter / Ctrl+Backspace on a focused proposal card.
- **F6 till assistant.** The full assistant in a drawer from the till header, with
  the open cart as DATA context (optional). Till shortcuts pause while it is open.
  Cashiers need `ai.use` (not granted by default).
- Tests: `crates/amwapos-hub/tests/ai_workspace.rs` (12), unit tests in
  `ai_workspace.rs` and `ai_stream.rs`, e2e "AI assistant" in `e2e/checkout.spec.ts`.

## AI hardening, 2026-09-27

Evidence: `crates/amwapos-hub/tests/ai_hardening.rs` (12 tests), `ai_admin.rs`, `ai_byok.rs`, `ai_workspace.rs`, `src/components/__tests__/WaQr.test.tsx`. Everything here ran offline (test model or a loopback stub). No real provider, WhatsApp or Windows Hello was used.

| Item | Status | Evidence | Pending |
| --- | --- | --- | --- |
| Proposal tools follow permissions, not the role name | Complete | A custom role named like the accountant but holding `prices.manage` can propose prices. A read-only role under any name gets no `propose_*` tool. A buyer gets PO/reorder only. | — |
| Strict write intent | Complete | "Should I enable loyalty?" records nothing. `/price …`, `/reorder …` and "set price of SKU X to 1.500" record a proposal. | — |
| WhatsApp connect Confirm card shows the QR image | Complete | The same `WaQr` component as the WhatsApp page (vitest). The QR and pairing code are never stored or sent to the model. | Live pairing with a real phone |
| Undo (compensating command) | Complete | price / bulk price / cost, archive-restore, loyalty ±, credit ± (flag), delivery status, cancel an unsent WhatsApp message, device rename. Restore, role, flag, backup restore and WhatsApp logout stay irreversible and link to their page. | — |
| Permission upgrade seeds | Complete | New permissions are granted once to built-in roles whose defaults include them. A permission the owner removes is not added back. Cashier never gets `ai.use`, `ai.mutate`, `admin.access` or `orders.manage`. | — |
| Rider `orders.manage` | Decision | The Delivery role includes `orders.manage` so riders can move digital orders to "out for delivery". The owner can remove it in Users → Roles (OPERATIONS soak list). | Owner review |
| Features checklist | Complete | Settings → Features shows "Default: off" on every module and a count of modules on. The setup wizard says optional modules start off. No flag defaults on. | — |
| Two-person control (flag `ai.dual_control`, default off) | Complete | Off: one confirm. On: high-risk proposals need a second, different person. The same person, even with DATA saying "approve", is refused. | — |
| B3 alerts | Complete | Refund spike, discount spike, negative stock, silent tills / dead letters, backup overdue. Checked every 5 minutes while the app is open, with thresholds in AI settings. One inbox alert per check per day. No writes. | — |
| B4 reorder / B5 margin price | Complete | Suggestions are reads. `propose_reorder` / `propose_margin_price` only record proposals, and confirm runs `po.save` / the price command. | — |
| B8 branch compare | Complete | `ai.branch_compare` returns `enabled:false` when `org.multi_branch` is off. | Real multi-branch data |
| C6 customer redaction | Complete | Name, phone and address are replaced by ids in tool results before provider HTTP. The loopback stub never sees them. | — |
| A9 answer language / C1 fast model | Complete | Settings (`ui`/`en`/`ar`, default `ui`). The fast model is used for triage and drafts, and is empty by default. | Real-provider quality |
| Hub bind | Complete | Listens on the chosen card, else the first LAN address, else 127.0.0.1. Never 0.0.0.0. | Store Wi-Fi soak |
| Idempotency on new writes | Complete | `loyalty.adjust` takes `operation_id` (same key + different payload is refused). Order convert refuses a key used on another order. Transfers check per step. Proposal confirm is an atomic status change. | — |
| Export guards | Complete | EOD zip CSVs neutralise formulas (test). The companion token appears only on the Confirm card, never in proposals, conversation, list, audit or diagnostics (test). | — |
| Print widths | Complete | The test page is 384 dots at 58 mm and 576 dots at 80 mm, with the Arabic line, and also renders to PDF (test). | Physical printer |

## Send loop (customer → WhatsApp → ticket → drop), 2026-09-27

One record chain: person → channel → **ticket** (a sent sale, or a digital order) → **drop** (the delivery row) → close. There is no third document type. It is additive: migration 15 adds `order_id`, `branch_id`, `channel` and `pay_state` to `delivery_orders`, plus `sale_collections` (append, synced) and `wa_chat_links`. Old delivery rows stay open and editable. Evidence: `crates/amwapos-core/tests/sendloop.rs` (6 tests), `wa_contacts` unit tests, `automation.rs`, and the e2e test "PAY Send with pay on delivery" in `e2e/layout1024.spec.ts`.

| Item | Status | Evidence | Pending |
| --- | --- | --- | --- |
| PAY: Here / Send | Complete | Here (the default) records the sale with no drop. Send needs a customer, and the drop is created in the same commit as the sale (one active drop per ticket). The address and area are prefilled from the customer and apply to this drop only, unless "Save on customer" is ticked. | — |
| Pay on delivery | Complete | Allowed only with Send. The sale commits with a `pay_on_delivery` tender, which is not cash: the drawer expects nothing and the ticket stays unpaid. Recording the money later (`tickets.record_payment`, idempotent) adds cash to the collecting shift's expected drawer and never changes the sale. Customer credit is not used. | — |
| One pay state | Complete | unpaid, recorded, screenshot_pending or paid, shown the same on the rail, board and WhatsApp header. A screenshot review in progress shows as screenshot_pending; a confirmed screenshot is recorded. No bank is settled. | — |
| Send rail on the till | Complete | Top-bar Send button (48×48) with a badge counting open drops for this person or nobody. A 420 px drawer on the assistant's side, above the dock, never covering PAY. Tabs To do, On the way and Done today, with 56 px rows. When empty: "Nothing to send. To deliver a sale, tap Send on the payment screen." | — |
| Ticket sheet | Complete | Lines are read-only. Shows the address, area, phone and a WhatsApp link, plus the pay chip, Record payment and Attach screenshot. Only the legal next steps appear (cancel needs `deliveries.manage`). Marking a ticket Delivered while unpaid asks "Paid?": take payment, or leave it unpaid (it then shows under Problem). Rider chips appear for managers, and Undo steps back one move. Message uses the existing templates with a confirm. | — |
| Cashier rights | Complete | With `pos.sell` a cashier can create a Send sale, open the rail, move their branch's drops forward and record payments. They cannot cancel, assign a rider, link chats or preview the phone's address book (tests). | — |
| Admin board | Complete | Columns New, Packing, On the way, Done and Needs help (out or delivered while unpaid, or a failed notice), using the same ticket sheet. Filter chips for pay state, channel, area and rider. The list view opens the same ticket sheet (the old drawer, which could mark an unpaid drop delivered without asking, was removed). | — |
| Digital orders (flag off by default) | Complete | Draft and confirmed orders wait in Now. Ring up uses the existing idempotent convert (stock moves once), and PAY prefills Send from the order. | — |
| WhatsApp header | Complete | Shows the customer's name, area, last ticket and pay chip, never the raw chat id. Tabs Chat, Tickets and Customer. A chat is linked by number first, then by hand (unmatched stays unmatched; test). New ticket needs a confirm and creates a draft only. With digital orders off: link the chat, then Start till sale. "Create order" is now "New ticket", and triage says "New ticket". A payment screenshot attaches by itself only when the person has exactly one open unpaid ticket. | Live WhatsApp soak |
| Area lexicon | Complete | 22 Bahrain places (English and Arabic spellings). Used on WhatsApp import, customer save and drops. "Maryam 1203/45 Riffa" gives address `1203/45` and area Riffa; with no match the area stays empty. | — |
| Notices | Complete | On-the-way and delivered notices carry the ticket, total, address and area; templates saved without `{address}` get an address line. Sent through the outbox with an idempotency key; a failure shows a banner on the ticket and nothing is rolled back. **Automatic notices are a new WhatsApp setting, off by default**; with it off a person taps Message. | — |
| AI | Complete | New read tools `list_open_drops` and `ticket_get`. No new mutation tool skips Confirm. | — |
| Flag defaults | Unchanged | `orders.digital`, `whatsapp.*` and `loyalty.enabled` stay off. | — |

## Product images, 2026-09-28 (hardened)

Precedence: uploaded picture > automatically found picture > monogram placeholder.

**Default and switches.**
- Automatic discovery is on by default. A new product saved without a picture is queued once (`pending`).
- Two switches turn it off:
  - the Settings → Product images checkbox "Find pictures automatically for new products" (`catalog.image_search.enabled`);
  - the administrator kill switch `AMWAPOS_IMAGE_SEARCH=off` (also `0`, `false` or `no`), which the setting cannot override.
- When discovery is switched off, disabled by the administrator, or has no usable source, new products stay `not_attempted`. Nothing is claimed, even work that was already queued. Manual uploads always work.
- The old `catalog.auto_images` feature flag was removed. A stored value is ignored.
- `.cargo/config.toml` sets `AMWAPOS_IMAGE_SEARCH=off` for processes started by cargo: tests, the e2e dev server and `tauri dev`. Tests and development therefore never contact real providers. Installed builds are not started by cargo, so they keep the default (on).

**Lifecycle** (`products.auto_image_status`, migration 0020):
- The states run `not_attempted → pending → processing → found | not_found | failed`. A manual upload or removal gives `skipped`.
- Terminal states are `found`, `not_found`, `failed` and `skipped`. A lifecycle is one logical discovery; `auto_image_attempts` counts retries within it.
- A temporary error (source or download outage, a Bing page that cannot be read, a timeout) goes back to `pending` after 5 min, then 1 h. The third temporary error gives `failed`.
- A claim abandoned for 10 min is recovered as a temporary error, so a lookup that keeps crashing ends `failed` rather than looping.
- Only an explicit person action starts a new lifecycle: "Find picture automatically", or the backfill for never-searched products. Reads, renders, edits, restarts and terminal sync never do (tested).
- Claiming is one conditional UPDATE. Completion writes only while the product is still claimed and has no picture, so an upload made during a lookup always wins (tested).
- The idle worker checks the queue with a read and writes only when work is due.

**Where it runs.**
- On the hub or a standalone till only. It never runs on a terminal (tested) and never on a read or render path.
- One product at a time, 3 s apart, 30 s idle poll. Product creation never waits for a source.

**Sources, in priority order** (`crates/amwapos-hub/src/image_worker.rs`):
0. **Default method: barcode + name on Bing's thumbnail address** (`bing_thumbnail`, on by default, no key). The query is "<barcode> <name>" (or name + category without a barcode), form-encoded into `https://tse1.mm.bing.net/th?q=<query>`. For example, "6767647641365 10 Colour Flame Candles" becomes `th?q=6767647641365+10+Colour+Flame+Candles`. No search page is read: that address is the single candidate, and Bing serves the picture it associates with the search when it is downloaded.
   - The download goes through the same SSRF-guarded fetcher and image checks (readable image, at least 200 px, not a strip).
   - The query is the identity, so no white-packshot frame is required.
   - When it gives a usable picture, that picture is used and no other source is asked (tested).
   - A tiny placeholder or an outage falls through to the sources below. An outage is a temporary error, never `not_found` (tested).
   - It is unofficial: Bing may change or limit it, and the picture is Bing's best guess for the words. A person can replace it at any time.
1. **Open Food Facts.** `GET https://world.openfoodfacts.org/api/v2/product/<barcode>?fields=…`, only when the product has an 8–14 digit barcode. No region parameter. Its results carry exact-barcode evidence.
2. **Bing images.** This is keyless, unofficial and best effort (the `bing-image-urls` approach). `GET https://www.bing.com/images/async` with:
   - `q`: the query text;
   - `first=0`, `count=35`, `adlt=strict`;
   - `qft=+filterui:photo-photo+filterui:imagesize-large+filterui:color2-FGcls_WHITE`;
   - `cc=BH`, `setmkt=en-BH` (or `ar-BH`), `setlang=en|ar`;
   - an `Accept-Language` header.

   These are market and language hints; Bing offers no "Bahrain only" filter. If the page contains result markup but nothing can be read from it, that is a temporary source error, never "no results", so a format change cannot mark products `not_found`.
3. **Google Programmable Search** (optional). `GET https://www.googleapis.com/customsearch/v1` with:
   - `key` and `cx`: the key is kept in the OS secret store and the cx in settings;
   - `q`, `searchType=image`, `num=10`, `imgSize=large`, `imgType=photo`, `imgDominantColor=white`, `safe=active`;
   - `gl=<market>` (a boost) and `hl=<language>`.

   There is no `cr`, which would restrict results to one country's documents. Google counts as a source only when both the key and the cx are set.

**How sources are combined.**
- Sources are asked in order. Each has its own 25 s budget, and a crash or bad reply is that source's error only (tested).
- Once a picture tied to the exact barcode is accepted, lower-priority sources are not asked (tested). Otherwise the best accepted picture across sources wins.
- At most 3 downloads per source and 6 per product.
- The search clients use the system proxy and follow redirects only to https on the same host, at most 3.

**Query and ranking.**
- The query text is "<barcode> <name>", or "<name> <category>" when there is no usable barcode.
- Candidates are refused outright when:
  - their title or page contains logo / banner / icon / vector / clipart / wallpaper / illustration / cartoon / recipe / poster / advert;
  - they are smaller than 200 px, or longer than 3:1;
  - they have neither barcode evidence nor half of the name words;
  - they are a web hit whose frame is not mostly white (white border < 0.45).
- Score weights, in strict priority:
  - exact barcode: 100;
  - name/brand word overlap: 40 × share;
  - picture quality: up to 3 in total (2 × white border + size up to 1);
  - GCC page domain (.bh .ae .sa .kw .qa .om): 0.3, a tie-breaker only.
- An exact global match therefore beats a partial Gulf match (tested).

**Downloads (SSRF).**
- Only http/https URLs, on ports 80/443, with no credentials, and no localhost / .local / .internal names.
- Every resolved address must be public. Refused ranges:
  - loopback, private, link-local (including 169.254.169.254 metadata), unspecified, broadcast, multicast, documentation;
  - 0/8, CGNAT 100.64/10, 192.0.0/24, 198.18/15 and 240/4;
  - on IPv6: unique-local, link-local, IPv4-mapped private and NAT64.
- The connection is pinned to the checked address, so DNS rebinding cannot change the target.
- Redirects are followed by hand, at most 3, and each one is checked again.
- 6 s connect and 15 s request timeouts; an `image/*` content type is required; an 8 MB streaming cap.
- The body is then decoded by magic bytes (PNG/JPEG/WebP/GIF only). Decoding is limited to 10 000 × 10 000 pixels and 256 MB of memory, and the image must be at least 64 px. The content type alone is never trusted.

**Proxy policy.**
- Image downloads never use the system proxy (`HTTP(S)_PROXY`, `ALL_PROXY`). Through a forward proxy, the proxy re-resolves the name and the pinned-address guarantee would not hold. Downloads therefore connect directly, and a one-time warning is logged if a system proxy is set.
- Where only proxied egress is allowed, an administrator can name a trusted proxy with `AMWAPOS_IMAGE_FETCH_PROXY=<url>`. The local address check still runs first, but final destination filtering then also depends on that proxy. This is a documented trust boundary, not a verified invariant.
- Both behaviours are tested with a fake proxy.

**Storage.**
- The normalised JPEG (at most 512×512, flattened on white, q85) is stored in `product_images`, keyed by its sha256. Identical pictures are stored once.
- A picture is deleted only when no product references it. Replace, remove and automatic results all happen in one transaction with the product row.
- Pictures are synced hub → terminals (`Policy::Hub`). Nothing is hotlinked.

**Backfill.**
- "Find pictures for products without one" (`products.manage`) queues at most 200 per press (the API allows up to 1000). It takes only active products with no picture that were never searched. It is idempotent and never touches manual pictures.
- The worker then paces the queue. Press again for the next batch.
- Nothing is queued on start-up, migration, import or page load.

**Terminals and the picture cache.**
- The hub writes the picture row before the product row that references it, and terminals apply changes in log order. So a terminal never sees a product whose picture has not arrived (tested on the sync log).
- The frontend has no sync event, so an unknown picture is simply asked for again after one minute. No realtime channel was added.

**Logging (tracing).**
- Events: queued (new product, request, backfill); lookup started; source asked or failed; candidate rejected (debug); candidate selected; picture persisted; manual picture won (superseded); outcome and terminal state; abandoned claims recovered.
- No keys (network errors never include URLs) and no image data are logged.

**Live check** (by hand, never in CI):
- Run `cargo test -p amwapos-hub --test image_live -- --ignored --nocapture`.
- It checks that Bing still returns parseable candidates, that Open Food Facts answers by barcode (5449000000996), and that a candidate downloads through the production fetcher.

**Limits.**
- Single business: access control is by permission (`products.manage`, `settings.manage`), not per tenant.
- Bing is unofficial and may change or rate-limit; a break only produces bounded retries.
- Imported products are not searched until the explicit backfill.

## WhatsApp Business catalogue, 2026-09-28

Publishes the POS catalogue to the WhatsApp Business catalogue of the linked number. It runs **through the existing WhatsApp link**: the same `whatsapp-rust` 0.7.0 client, session file, supervisor and hub/standalone runtime that send receipts. It adds no second login, no Meta Graph or Cloud API, and no other provider.

**How it talks to WhatsApp.**
- New `AdapterSession` methods, with defaults that report "unsupported":
  - `catalog_capability`, `catalog_upload_image`;
  - `catalog_create`, `catalog_update`, `catalog_delete`, `catalog_list`.
- `RustWhatsAppAdapter` implements them with the client's public primitives:
  - `get_business_profile` (Business detection);
  - `send_iq` with `w:biz:catalog` stanzas: `product_catalog_add`, `product_catalog_edit`, `product_catalog_delete` and the `product_catalog` read;
  - `upload(…, MediaType::ProductCatalogImage)` for pictures.
- These are the stanzas WhatsApp Web uses for its own catalogue. The `Baileys` library is the reference implementation (`crates/amwapos-hub/src/whatsapp/catalog_proto.rs`).
- The library's GraphQL catalogue operations are read-only, and there is no collection write stanza. So **Collections are not written**: POS categories are recorded as `unsupported` collections, and products are published without collection membership.

**Capability, from the live connection.**
- The states are `disconnected`, `checking`, `personal`, `business_no_catalog`, `supported`, `unavailable` and `terminal`.
- `personal` means no business profile. `business_no_catalog` means a business profile exists but the catalogue read is refused; create the catalogue once in the WhatsApp Business app.
- `supported` means the business profile exists and the catalogue read succeeded.
- The capability is re-checked on every new client or relink, every 30 min, or when "Check again" is pressed.
- Sync is refused unless the capability is `supported` **and** was checked for the account that is linked right now.

**Direction and ownership.**
- POS → WhatsApp only. WhatsApp edits never change POS data.
- The worker only touches remote products it created or adopted.
- Adoption: before creating an unmapped product, the account's catalogue is consulted (a remote index kept in step with the worker's own writes, re-read after 10 min, a failed create, a "not found", a relink or a full sync's remote check). A remote product whose retailer id equals the POS product code and that is not mapped to another product is updated instead of duplicated; with several such copies the oldest is adopted. This also makes an interrupted run resumable.
- If the catalogue cannot be read, the create waits; it never runs blind.
- Remote products created in the WhatsApp app are never modified or deleted.

**Mapping (migration 0021).**
- `wa_catalog_products` is keyed by (account = linked number digits, product_id). It holds:
  - the remote id;
  - the synced fingerprint and the attempted fingerprint;
  - the uploaded picture hash and URL;
  - retries, timestamps and the last error.
- `wa_catalog_collections` is keyed by (account, category_id).
- These tables are local to the computer owning the session (not in the row sync).
- Names are never identifiers.
- A different linked number starts with no mappings and needs its own first sync. The old account's rows are kept as history and never used.

**What is sent.**
- Name: one line (control characters and repeated spaces removed), at most 150 characters, cut deterministically with "…". Descriptions keep single line breaks.
- A hidden product that lost its price keeps its last published price.
- Description: the product description, else the Arabic name, else none (nothing invented); at most 1000 characters.
- Price in thousandths of the currency (the protocol's amount×1000). For BHD, fils map 1:1: 0.100 → 100, 1.250 → 1250, 9.990 → 9990, 100.000 → 100000. Integer arithmetic only.
- Currency code, retailer id = the POS product code (SKU), hidden flag.
- Picture: the POS-managed stored JPEG (uploaded or automatically found), sent byte for byte. It is re-uploaded only when its hash changes (an upload is reused by the retry of a failed write for 24 h). Outside search URLs are never used, and a product without a picture is published without one. A missing, non-JPEG or refused picture publishes the product without it (shown on the product) instead of failing it.
- These character limits are AMWAPOS' conservative choice; WhatsApp does not publish its limits.

**Eligibility.**
- Published: active products with a name and a price above zero.
- Not published: archived products and products without a price. A published product that is archived or loses its price is **hidden** on WhatsApp, not deleted.
- A mapping whose POS product no longer exists deletes only its POS-owned remote product.

**Lifecycle.**
- `queued → syncing → synced | hidden | removed | failed | remote_missing`.
- Temporary errors (disconnect, timeout, 429 with the server's back-off, 5xx) retry after 1, 5, 30 and 120 min; the 5th failure gives `failed`.
- `failed` is terminal until the product's representation changes, or an administrator presses Retry. There is no loop.
- `remote_missing` (deleted on WhatsApp) is re-created only by Retry or an administrator's full sync, never by the automatic sync.
- A claim abandoned for 10 min is taken back; a restarted worker releases all claims at once.
- An edit made while that product's write is in flight is re-queued, never recorded as published.
- A full sync is a run (`wa_catalog_runs`, migration 0024): its progress is shown, and once per run the remote catalogue is read to find mapped products deleted on WhatsApp.
- An unchanged fingerprint means no remote write.

**When it runs.**
- Nothing is published on upgrade. An administrator starts it with "Sync catalogue to WhatsApp" (WhatsApp → Catalogue), per linked number.
- After that, "Keep the WhatsApp catalogue synchronised automatically" is on by default. It compares POS and WhatsApp after every catalogue command (products, categories, pricing, import) and at least once a minute.
- With it off, changes stay local until the next manual sync.
- The worker runs next to the message I/O worker, only where the session is (hub / standalone, never a terminal). It processes 5 products per claim, one remote change at a time, 1.2 s apart.
- POS writes never wait for it. While disconnected, work stays queued and resumes on reconnect without duplicates.

**Permissions and logging.**
- Sync, retry, the setting and re-check need `whatsapp.manage` **and** `products.manage`. Status needs `whatsapp.manage`; the product line needs `products.view`. Cashiers can do none of these.
- Session material never leaves the backend.
- Logs cover capability detected, changes queued, product created / updated / hidden, not synchronised (with the error), abandoned claims re-queued and full sync started. No session keys or tokens are logged.

**Verified here.**
- Core state machine: `crates/amwapos-core/tests/wa_catalog.rs`.
- End to end with the fake adapter: `crates/amwapos-hub/tests/whatsapp_catalog.rs`.
- Stanza building and parsing: unit tests in `catalog_proto.rs`.
- Hardening audit 2026-09-29 (17 findings, fixes, invariants, state machine): `docs/WA_CATALOGUE_AUDIT.md`. Golden stanza fixtures in `catalog_proto.rs`; load, relink, duplicate, refused-picture, rapid-press and permission tests in the hub suite.
- **Not verified live**: no WhatsApp Business account was available in this environment. Business detection, catalogue access, product create/update with picture and BHD price, and re-sync without duplicates must be checked once on a real linked Business number. Collections are unsupported by design (blocked).

## Supplier documents and WhatsApp orders, 2026-09-29

Architecture, flows, permissions, AI boundary and limits: `docs/DOCUMENTS_AND_ORDERS.md`.

**Built.**
- **Document Intelligence.**
  - Images and PDFs; a quality check with preprocessing and rotation; layout OCR with word boxes (English and Arabic).
  - Classification of invoices, credit notes and delivery notes.
  - Every field with provenance and a confidence band.
  - Supplier and product matching that learns from people's corrections.
  - Deterministic arithmetic and VAT checks, duplicates, anomaly signals, and PO and three-way reconciliation.
  - A review screen with evidence boxes.
  - Supplier invoice records (review only) and receiving drafts, posted by a person through normal receiving.
- **WhatsApp AI orders.**
  - Customer chats on the existing link become draft digital orders with real products, prices and stock.
  - Clarifying questions with real options, Bahrain addresses, and a fee from the delivery zones.
  - Staff reply, take over, correct (learned aliases), verify payment screenshots, confirm or cancel.
  - Nothing is sent, charged or moved by itself.
- **Optional AI.** For both features the AI fills only unresolved values. Its output is schema-validated and never overrides a person.
- **Assistant tools.** Read tools for all of this in the assistant's tool map.

**Verified here.**
- Core, hub and real-OCR tests, and the runtime test with the fake WhatsApp adapter.
- Playwright end to end for both flows: real Tesseract on a synthetic invoice image, then receiving by a person; a chat with a question, then staff reply and confirmation.
- The evaluation set: 9 documents and 16 chats, 100% per field, with no unsafe result.
- Clippy, rustfmt, typecheck, lint, vitest and build.

**Not verified live.**
- A real AI provider reading documents or chats. Validation and fallback are tested, with the offline provider.
- A real WhatsApp number receiving customer chats. This uses the same link that is already live-unverified for catalogue sync.
- Real supplier documents. The evaluation data is synthetic by design.

**Limits.**
- Text PDFs have no evidence boxes.
- JBIG2 and CCITT images inside PDFs are not decoded.
- Handwriting is best-effort.
- Image barcodes are not decoded.
- Supplier liabilities are posted only from the Payables page, by a person (see "Platform pass" below).

## Platform pass, 2026-10-03

Evidence (this environment, Linux): `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace` (376 passed, 0 failed, 4 ignored: the benchmark and live-network checks),
`tsc`, `eslint`, `prettier --check`, vitest (48), `vite build`, Playwright (13 flows), and the 100k-product
benchmark (release build: P95 scan 0.74 ms, search 4.6 ms, cart 0.93 ms, sale commit 6.8 ms). CI
(Linux + Windows build + installer smoke) was green on `c94a9ab` before this pass; later commits
are on the same branch.

| Item | Status | Evidence | Pending |
| --- | --- | --- | --- |
| AI action classes (read … financial / external) enforced in the backend; automated workflows capped at Draft | Complete | `ai_actions.rs`; `ai_hardening.rs` "action classes are enforced in the backend" | — |
| WhatsApp orders forensic pass (negation, cancels, stale sessions, late messages, AI limits, confirm re-checks stock and quoted total) | Complete | `tests/waorders_forensic.rs` (16), `tests/waorders.rs` | Live WhatsApp soak |
| Stock holds for confirmed orders (migration 0025): free stock only, released on cancel, converted on sale, expire after 48 h | Complete | `waorders_forensic.rs` expiry test; `orders.rs` | Holds are kept on the computer that confirmed the order; they are advisory and never block a till sale |
| Accounts payable (migration 0026): post supplier invoices / credit notes, payments, allocations, derived balances, ageing, reversal, immutability, idempotency, `payables.*` permissions | Complete | `tests/payables.rs` (10) | — |
| Payables page (Purchasing → Payables): owed / overdue / due in 7 days, ageing, suppliers, invoices waiting, supplier account + statement, payment filled oldest first, post and reverse with confirmations, manual invoice entry | Complete | e2e "payables: … reviewed, posted and paid" | — |
| Back office stays on the hub: payables and supplier-document writes refuse on a terminal (their tables do not sync) | Complete | `tests/sync.rs` "payables and supplier documents are hub only" | — |
| Order journey UI: one "Orders & delivery" menu group, journey bar (`orders.flow`), next-step card on WhatsApp orders, "Confirm anyway" for a shortage or a changed total, plain status words, rider next-step button | Complete | vitest `orderJourney.test.ts`; e2e WhatsApp order; `waorders_forensic.rs` guide-bar test | — |
| Dashboard "Needs attention": orders to confirm, payment screenshots, overdue supplier invoices, invoices ready to post (only for people who may act) | Complete | `tests/payables.rs` dashboard test | — |
| Arabic: every backend error message translated | Complete | vitest `backendErrors.test.ts` scans every `AppError` in the Rust sources (> 500 messages) | — |
| Arabic: money fields kept their value (an invisible direction mark made the parser fail; a delivery-zone fee became 0) | Complete (fixed) | vitest money tests, including a source scan that forbids cutting numbers out of formatted money | — |
| Settings forms: plain translated labels, % for rates, money as amounts; supplier price-variance threshold named | Complete | vitest i18n coverage | — |
| Till: quick-cash buttons offer amounts that cover the bill; Latin names in the Arabic cart cut at their end; stock chip never clipped | Complete | vitest `quickCash`; 1024×768 Arabic screenshot (layout spec) | Visual check on the till panel |
| Admin menu: modules that are off are not listed (their pages still say "Not enabled"); rarely used system pages under "More tools" | Complete | e2e flows open "More tools" before Audit / Diagnostics | — |
| Product pictures: default source is barcode + name on Bing's thumbnail address | Complete in code | unit + fixture-server tests | Live check (Bing was not reachable from this environment) |
| Stocktake cancel and supplier-record void ask first and say they cannot be undone | Complete | — | — |

## Product / UX forensic pass, 2026-10-03

Every user-facing surface is inventoried in [PRODUCT_UX_FORENSIC_AUDIT.md](PRODUCT_UX_FORENSIC_AUDIT.md):
92 screens with the 15 audit fields (setup and sign-in, cashier, 48 Admin destinations and record
pages, 18 Settings sections) and 73 grouped rows for the 151 dialogs, drawers and confirmations.

Evidence (this environment, Linux): `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace`
(376 passed, 0 failed, 4 ignored), `tsc`, `eslint`, `prettier --check`, vitest (49), `vite build`,
Playwright (15 flows, including the new English/Arabic sweep of every Admin page and Settings section
and the role sweep), and the 100k-product benchmark (release build: P95 scan 0.80 ms, search 4.10 ms,
cart 0.55 ms, sale commit 6.37 ms).

| Item | Status | Evidence | Pending |
| --- | --- | --- | --- |
| Every Admin page and Settings section: heading, loads, no error, no sideways scroll, no English in Arabic, deep links open their section | Complete | e2e `surfaces.spec.ts` (0 findings, EN + AR) | — |
| Manager, accountant and inventory roles: every link shown opens without a permission error | Complete | e2e role sweep | — |
| Settings deep links (`?section=`) work while Settings is open | Complete (fixed) | e2e sweep asserts the active section | — |
| Admin mode survives a reload / language switch for the session | Complete (fixed) | e2e product-pictures flow | — |
| Supplier invoices have one home (Payables, with Void for unposted records) | Complete | e2e payables flow | — |
| Plain, sentence-case labels; no-break table dates; date-range "Custom dates"; readable Admin connection pill; one page heading per screen | Complete | vitest `time.test.ts`; sweep | Visual check on the till panel |

## Merchant OS Wave 1: money after the sale, 2026-10-05

Plan: [MERCHANT_OS_PLAN.md](MERCHANT_OS_PLAN.md). Design and rules: [FINANCE.md](FINANCE.md).

Evidence (this environment, Linux):
- `cargo fmt --check`, `clippy -D warnings`;
- `cargo test --workspace`: 398 passed, 0 failed, 4 ignored (the benchmark and live-network checks);
- `tsc`, `eslint`, `prettier --check`, vitest (49), `vite build`;
- Playwright: 18 flows, including the 3 new Wave 1 flows and the English/Arabic sweep, which now
  covers Expenses and the new reports;
- 100k-product benchmark (release build), P95: scan 0.96 ms, search 4.88 ms, cart 0.89 ms,
  sale commit 9.16 ms. The commit now also writes the receipt snapshot; the target is 500 ms.

| Item | Status | Evidence | Pending |
| --- | --- | --- | --- |
| Trading-day cutoff (0–6 h) used by sales, refunds, shifts, EOD, reports, AI and briefings | Complete | `wave1.rs` cutoff test; `time.rs` unit tests | — |
| Issued receipts stored with SHA-256; reprints are exact copies; older records marked reconstructed | Complete | `wave1.rs` (2); `sync.rs` snapshots reach the hub | Printed copy check on the soak printer |
| Refund and void approvals bound to one exact request, with the summary kept on the server | Complete | `auth.rs` unit test; `flow.rs` bound refund; `wave1.rs` cashier void | — |
| Void of a whole sale (today, open shift, nothing refunded or delivered), reason required, restock, tenders and account credit returned | Complete | `wave1.rs` (4); e2e "void at the till" | — |
| Every table classified as replicated or hub-local | Complete | `wave1.rs` replication-decision test | — |
| Expenses: categories, VAT, attachments, approval (limit), payment (petty cash, till paid-out linked once, bank, card, cheque), void, repeats as drafts | Complete | `wave1.rs` (5); e2e "expense … operating profit" | — |
| Petty cash funds with immutable entries, counts and adjustments | Complete | `wave1.rs` petty cash test | — |
| Operating profit report (explicitly not net profit), Expenses report, refunds report shows voids | Complete | `wave1.rs` lifecycle test; e2e | — |
| Customer statements (bilingual PDF, WhatsApp), ageing with per-customer days to pay, Receivables report | Complete | `wave1.rs` statements test; `credit.rs` ageing unit tests; e2e statement + PDF | Live WhatsApp send |
| Expenses and petty cash hub-only; refused on a terminal | Complete | `sync.rs` hub-only test | — |
| AI: read tools for expenses, petty cash, statements, receivables and void eligibility; voids, payments and approvals stay with people | Complete | `ai_admin.rs` command coverage; `ai_actions.rs` classes | — |
| Migration 0027: one `stock_movements` rebuild checked for unchanged rows and quantities; nothing invented for the past | Complete | migration tests; `wave1.rs` | Upgrade of a real store copy during the soak |

## Merchant OS Wave 2: the trading day, 2026-10-05

Design and rules: [FINANCE.md](FINANCE.md), "Closing the trading day".
Branch `claude/amwapos-merchant-os-wave2`, based on `main` at `e8204ff` (Wave 1).

Evidence (this environment, Linux):
- `cargo fmt --check`, `clippy -D warnings`;
- `cargo test --workspace`: 413 passed, 0 failed, 4 ignored (the benchmark and live-network checks);
- `tsc`, `eslint`, `prettier --check`, vitest (49), `vite build`;
- Playwright: 19 flows, including the Wave 2 flow and the English/Arabic sweep, which now covers
  End of day, Cases and Registers;
- 100k-product benchmark (release build), P95: scan 1.16 ms, search 5.07 ms, cart 0.66 ms,
  sale commit 8.83 ms. Nothing from Wave 2 runs during a sale.

| Item | Status | Evidence | Pending |
| --- | --- | --- | --- |
| X report (current totals): sales, refunds, voids, discounts, VAT, tenders, cash and non-cash, pay on delivery, account sales, drawers, expected / counted / difference, cash in and out, safe drops, delivery collections; writes nothing | Complete | `wave2.rs` X test (row counts, change log and `total_changes()` unchanged) | — |
| Z close: one per branch and business date; idempotent (retry, lost response, duplicate, mismatched id); stored snapshot + document + SHA-256; immutable; independent of later settings; PDF from the stored document | Complete | `wave2.rs` (3) | Printed copy on the soak printer |
| Records arriving after their day was closed: kept with their own date and time, closed day unchanged, counted once in the next close as after-close adjustments | Complete | `wave2.rs` late-sale test; `sync.rs` till offline during the close | — |
| Days close in order; Wave 1 cutoff reused (no second calculation); boundary at the cutoff | Complete | `wave2.rs` (2) | — |
| Branches close separately | Complete | `wave2.rs` branch test | — |
| Closing checks as blocking / warning / information; opening checklist that never blocks | Complete | `wave2.rs`; e2e | — |
| Registers and drawers; shift records both; migration from schema 27 keeps every shift and attributes none | Complete | `wave2.rs` (2) | Replacing a computer on site |
| Cash-difference cases: facts, steps, notes, files, outcomes, permanent history; one per shift; made on the hub when a terminal's shift arrives | Complete | `wave2.rs` case test; `sync.rs`; e2e | — |
| Permissions `day.x_report`, `day.close`, `registers.manage`, `cases.view`, `cases.manage`, `cases.resolve`; terminals refuse to close or act on cases | Complete | `wave2.rs` permissions; `sync.rs` | — |
| Failure handling: close interrupted half-way (nothing kept, retry closes once), repeated cash count, repeated case step, duplicate case | Complete | `wave2.rs` | — |
| AI reads X, checks, closes, opening, cases and registers; it never closes, resolves or changes counts | Complete | `ai_admin.rs` coverage; `ai_actions.rs` | — |
| English and Arabic for every new screen, message and check | Complete | vitest coverage; e2e sweep | Visual check on the till panel |

Note: `crates/amwapos-hub/tests/whatsapp_catalog.rs` "unreachable capability check" timed out
once during a fully parallel workspace run. It has a 30-second polling limit and passed on its own
and in the final full run. It is not related to Wave 2.

## Merchant OS Wave 3: inventory truth, 2026-10-05

Design and rules: [INVENTORY.md](INVENTORY.md).
Branch `claude/amwapos-merchant-os-wave3`, based on `main` at `586fe10` (Wave 2).

Evidence (this environment, Linux):
- `cargo fmt --check`, `clippy -D warnings`;
- `cargo test --workspace`: 428 passed, 0 failed, 4 ignored (the benchmark and live-network checks);
- `tsc`, `eslint`, `prettier --check`, vitest (49), `vite build`;
- Playwright: 20 flows, including the Wave 3 flow and the English/Arabic sweep, which now covers
  Expiry, Waste and Days of stock left;
- 100k-product benchmark (release build), P95: scan 0.89 ms, search 4.99 ms, cart 0.73 ms,
  sale commit 9.26 ms (Wave 2: 8.83 ms; Wave 1: 9.16 ms). Nothing was added to the sale path, so
  this is run-to-run noise.

| Item | Status | Evidence | Pending |
| --- | --- | --- | --- |
| One stock truth: a batch has no quantity of its own; balances are replayed from movements; batches + stock in no batch = stock on hand | Complete | `wave3.rs` (every test asserts it) | — |
| Batches from receiving (direct, PO, drafts), retry-safe; old stock left in no batch; nothing invented at upgrade | Complete | `wave3.rs` receiving and upgrade tests | — |
| First-expiring-first-out estimate for sales without a batch (older stock first, ties by received date then id), never stored, labelled as an estimate; recorded evidence moves it | Complete | `wave3.rs` (3) | — |
| Offline tills sell without batches; hub batches converge; arrival order does not matter | Complete | `sync.rs` offline till + waste; `wave3.rs` order test | Real store Wi-Fi |
| Expiry states (expired / past best-before / urgent / soon / later), expired is not disposed, at-risk vs already-lost figures, markdown scenarios as options only | Complete | `wave3.rs`; e2e | — |
| Date checks: impossible refused, suspicious need confirming; document-read dates need a person | Complete | `wave3.rs` (2); `lots.rs` unit tests | Real supplier labels and documents |
| Waste: reasons incl. neutral shrinkage, one movement per operation, approval above value or for shrinkage (bound), reversal by compensation, record kept | Complete | `wave3.rs` (2) | Store disposal routine |
| Waste summary with defined ratios; days of stock left (7/30/60/90, holds, stock on order apart, no stock / no demand / not enough history, branch-scoped, refunds and voids counted as in reports) | Complete | `wave3.rs` days-of-stock test; `lots.rs` unit tests | — |
| Permissions `lots.manage`, `waste.record`, `waste.approve`; hub-only tables; AI reads only | Complete | `wave3.rs`; `sync.rs`; `ai_admin.rs` | — |
| English and Arabic for all new screens, messages, permissions and settings | Complete | vitest coverage; e2e sweep | Visual check on the till panel |

## Merchant OS Wave 4: procurement, 2026-10-06

Design and rules: [PROCUREMENT.md](PROCUREMENT.md).
Branch `claude/amwapos-merchant-os-wave4`, based on `main` at `058a65d` (Wave 3).

Evidence (this environment, Linux):
- `cargo fmt --check`, `clippy -D warnings`;
- `cargo test --workspace`: 457 passed, 0 failed, 4 ignored (the benchmark and live-network checks);
- `tsc`, `eslint`, `prettier --check`, vitest (49), `vite build`;
- Playwright: 21 flows, including the Wave 4 flow (Suggested orders → requisition → approve →
  purchase order → place → receive with a refusal and a shortage → differences → supplier
  return) and the English/Arabic sweep, which now covers Suggested orders, Requisitions,
  Supplier returns, a new return, Settings → Purchasing and the supplier performance report;
- 100k-product benchmark (release build), P95: scan 1.23 ms, search 4.79 ms, cart 0.75 ms,
  sale commit 9.73 ms (Wave 3: 9.26 ms). Nothing was added to the sale path; this is run-to-run
  noise. Suggested orders over 100,000 products with three suppliers each (50,000 to order):
  1.58 s. The dashboard's "products to order" count is a five-minute hint, so a cold dashboard
  at that size takes about 0.8 s once, and Suggested orders is always computed live.

| Item | Status | Evidence | Pending |
| --- | --- | --- | --- |
| Supplier catalogue: one model (terms in `supplier_products`, document aliases in `supplier_product_map`, unchanged); person-confirmed pack wins; exact pack arithmetic; zero/negative packs refused | Complete | `wave4.rs`; `catalogue.rs` unit tests | Real supplier terms |
| One replenishment engine on the shared demand functions; set-based; holds and expired use-by excluded; orders, drafts, transfers and requisitions counted once; explicit states; reason codes; deterministic supplier choice with alternatives; never orders | Complete | `wave4.rs`; `replenish.rs` unit tests; benchmark | — |
| Requisitions: lifecycle, provenance with evidence, one draft PO per supplier, exactly once (retry, concurrency), whole requisition only, rollback on failure | Complete | `wave4.rs` (4) | — |
| PO approval: off / always / above an amount; durable, bound to the material details; invalidated by a material edit; off keeps the old flow | Complete | `wave4.rs` | — |
| Receiving: delivered, accepted, refused (not stock, not waste), damaged kept, substitute (A → B, explicit), shortage (backorder / cancel), overage (confirmed; approval beyond tolerance); batches; retry / changed / partial / failure part way / concurrent | Complete | `wave4.rs` (6) | Real back-door checking |
| Three-way match in the existing reconcile: cumulative accepted vs posted invoices, pack conversion, VAT, tolerances (amount and %), matched / within tolerance / review / blocked; AP posting gated; acceptance bound to the result | Complete | `wave4.rs` (2) | Real supplier invoices |
| Supplier returns: one lot-aware movement per line, quantity checks in the same transaction, reversal by compensation, expected vs actual credit, credit note link without a second stock effect | Complete | `wave4.rs` (3) | Real credit notes |
| Permissions `requisitions.create`, `purchasing.approve`, `supplier_returns.manage`; owner removals stick; hub-only tables; AI reads only, reorder drafts a requisition | Complete | `wave4.rs`; `sync.rs`; `ai_admin.rs`; `ai_hardening.rs` | — |
| Upgrade from schema 29 invents no terms, approvals, differences or returns | Complete | `wave4.rs` upgrade test | — |
| Supplier performance (lead time set vs seen, fill, short, extra, damaged, refused, returned, cost difference, invoices reviewed); dashboard items that link to their screens | Complete | report and dashboard code; e2e sweep | — |
| English and Arabic for all new screens, messages, permissions and settings | Complete | vitest coverage; e2e sweep | Visual check on the back-office panel |

Commits were grouped as domain, screens, then docs and evidence (three commits rather than the
six suggested checkpoints): the domain modules share one migration and module registration, so
splitting them further would have left commits that do not build.

### Wave 4 resume and verification, 2026-10-06

- Lineage: branch `claude/amwapos-merchant-os-wave4`, base `058a65d` (= `main`), Wave 4 commits
  only, nothing on `main` since.
- CI on `10626f7` failed in `cargo clippy` only: CI uses Rust 1.99.0, whose clippy flags
  `single_element_loop` in `replenish.rs` (local 1.98.1 did not). Reproduced with 1.99.0, fixed
  in `45201f9`. Frontend, dependency audit and the end-to-end job passed on `10626f7`.
- Defect found and fixed: receiving on a purchase order had no way to keep a suspicious batch
  date (for example an expiry already passed); it now shows the date warnings and keeps the dates
  on a second press, as direct receiving does. `e2e/wave4.spec.ts` now covers it.
- Evidence on `45201f9`, Rust 1.99.0 (the CI toolchain): `cargo fmt --check`, `cargo clippy
  --workspace --all-targets -D warnings` clean; `cargo test --workspace`: 457 passed, 0 failed,
  4 ignored. `tsc`, `eslint`, `prettier --check`, vitest 49 passed. Playwright 21 of 21 passed,
  including the Wave 4 flow and the English/Arabic sweep of every Admin page.
- The benchmark figures above are from the release run of the Wave 4 code before the dashboard
  hint was added; the hint changes only the dashboard, not the sale path or the engine.
- - CI on `43b4a84`: Windows build + tests + installer, frontend, end-to-end and dependency audit
  passed; the Linux Rust job failed in one pre-existing hub test,
  `an_unreachable_capability_check_is_retried_slowly_and_publishes_nothing` (the file is not
  changed by Wave 5). Root cause: a race in the test. The catalogue worker may check the
  capability (supported) while the test environment starts, before the test switches the fake to
  failing; the once-a-minute re-check limit then holds "supported" longer than the test waits.
  It passed 6 of 6 locally and on Windows. Fixed in the test by asking for "check again" after
  switching (honoured at once), keeping every assertion, including the once-a-minute limit.
- CI on the final head: pending at the time of writing.

## Merchant OS Wave 5: retail commercial foundation, 2026-10-06

Design and rules: [PRICING_AND_CATALOGUE.md](PRICING_AND_CATALOGUE.md).
Branch `claude/amwapos-merchant-os-wave5`, based on `main` at `c681877` (Wave 4, fast-forwarded).
Seven checkpoint commits plus two end-to-end fixes found by the full suite.

Evidence (this environment, Linux, Rust 1.99.0 = CI toolchain):
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` clean;
- `cargo test --workspace`: 506 passed, 0 failed, 4 ignored (benchmark and live-network checks);
- `tsc`, `eslint`, `prettier --check`, vitest (49), `vite build`;
- Playwright 22 of 22, including the Wave 5 flow (scale rule and label test → duplicate merge →
  pricing review apply → scale label on a phone order) and the English/Arabic sweep, which now
  covers Likely duplicates, Scale barcodes, Pricing review and Pricing policies;
- 100k-product benchmark (release build), P95: scan 1.57 ms, scale-label scan (200 rules)
  1.45 ms, PLU scan 1.39 ms, search 5.16 ms, sale commit 11.64 ms (Wave 4: 9.73 ms; the sale path
  gained only a cart channel read and line evidence columns). Duplicate review 0.99 s and pricing
  review 0.91 s over 100,500 products.
- Found by the full end-to-end suite and fixed: the address form's "More details" link was below
  the 48 px touch size on the till; two new nav items produced a second "More tools" control.

| Item | Status | Evidence | Pending |
| --- | --- | --- | --- |
| Barcode kinds chosen, never guessed; legacy "Not recorded" | Complete | `wave5.rs` | — |
| PLU: unique, normalized, cross-conflicts with barcodes refused naming the owner; scan and search | Complete | `wave5.rs` (3) | — |
| Scale barcodes: validated rules, index lookup, exact barcode wins, fail closed on ties, integer weight/price, sanity limits, evidence on lines, price kept on restore, offline via hub | Complete | `wave5.rs` (7); `sync.rs`; benchmark | Real scale printers |
| Likely duplicates: blocking keys, evidence, normalization, Zero/size guard, Not duplicates / Review later | Complete | `wave5.rs` (2); `merge.rs` unit tests; benchmark | Real catalogues |
| Product merge: preview, blockers, explicit choices, stock and batch conservation, history untouched, idempotent, stale preview refused, atomic, owner only | Complete | `wave5.rs` (5) | Real merges with history |
| Addresses: governorate, directions, normalized area; WhatsApp never overwrites a saved address | Complete | `wave5.rs`; `waorders.rs`; `address.rs` unit tests | — |
| Sales channel: explicit, before pricing, kept through hold, immutable, "Not recorded" for old sales, separate from fulfilment | Complete | `wave5.rs` (2) | — |
| Channel prices through the one resolver, documented fallback, "Using retail price", snapshot on the sale, order estimate = sale | Complete | `wave5.rs` (4); `sync.rs` | — |
| Pricing policies, precedence, ambiguity surfaced, markup vs margin, explicit cost basis; recommendations only | Complete | `wave5.rs` (3); `policies.rs` unit tests | Real supplier costs |
| Rounding never below the minimum margin (exhaustive property test), VAT-inclusive correct | Complete | `policies.rs` unit tests | — |
| Margin review groups, dismiss/postpone, bulk apply atomic/idempotent/audited with bound approval, history only for applied changes | Complete | `wave5.rs` (2); e2e | — |
| Reports: sales by channel (no fake channel profit), price changes; dashboard cards | Complete | `wave5.rs` | — |
| Permissions `catalog.merge` (owner), `pricing.policy`, `barcode_rules.manage`; removals stick; cashiers none | Complete | `wave5.rs` | — |
| AI: reads only; Wave 5 writes have no tool; old tools cannot set PLU/governorate/channel; never below minimum margin | Complete | `wave5.rs`; `ai_admin.rs` | — |
| Upgrade from schema 30 fabricates nothing; batches copied exactly | Complete | `wave5.rs` upgrade test | — |
| Ten invariants together | Complete | `wave5.rs` invariants test | — |
| English and Arabic for every new screen, message, permission and setting | Complete | vitest; e2e sweep | Visual check on real tills |

- CI on `43b4a84`: Windows build + tests + installer, frontend, end-to-end and dependency audit
  passed; the Linux Rust job failed in one pre-existing hub test,
  `an_unreachable_capability_check_is_retried_slowly_and_publishes_nothing` (the file is not
  changed by Wave 5). Root cause: a race in the test. The catalogue worker may check the
  capability (supported) while the test environment starts, before the test switches the fake to
  failing; the once-a-minute re-check limit then holds "supported" longer than the test waits.
  It passed 6 of 6 locally and on Windows. Fixed in the test by asking for "check again" after
  switching (honoured at once), keeping every assertion, including the once-a-minute limit.
- CI on the final head: pending at the time of writing.
