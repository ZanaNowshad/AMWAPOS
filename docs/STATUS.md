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

Evidence was re-run on 2026-09-24:
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
| Backup / verified restore / safety backup | Complete | Round-trip and tamper tests; E2E backup | USB / network-share folder checked in the soak (OPERATIONS checklist) |
| Backup-while-closed | Complete (operational rule) | OPERATIONS "Backup rule" (the hub keeps AMWAPOS running). Red banner on every Admin page and a till header pill, with one-click Backup Now. `backup.health` reports ok / overdue / failed. | A Windows scheduled task was deliberately not built (reasons in OPERATIONS) |
| Diagnostics | Complete | Readable details; last backup shown in local time with age; sync errors classified | — |
| Sync protocol v2 (encrypted, SPAKE2 pairing, one live code, burn after 5, pairing-id reuse rejected, versioned) | Complete | Encrypted-channel tests: proxy test, protocol 1 refused, burn, version mismatch shown as "Update needed", not offline | — |
| Lost hub credential | Complete | Reported, never silently replaced; owner reset + re-pair (sync test) | — |
| Multi-terminal operation on store Wi-Fi | Partially complete | Sync convergence tests; signed + encrypted HTTP tests on loopback | Two tills + hub on store Wi-Fi; unplug the hub mid-sale |
| Printing: receipt, COPY reprint, cash-drawer pulse, failure keeps sale | Partially complete | `tests/printing.rs`: receipt content and width; COPY; drawer pulses on cash only (not card, not reprint, not when off); failed printer keeps the sale and retry works; ESC/POS framing | 80 mm printer + drawer (network and Windows spooler) |
| Arabic receipts | Partially complete | Raster unit tests (shaping, lam-alef, bidi, placement); sale/refund receipts with Arabic names and bilingual labels; no `?` in text mode; test page has an Arabic line | Arabic on the physical printer |
| Barcode scanner (HID wedge) | Partially complete | Vitest heuristics; E2E burst of 5 scans at scanner speed, none dropped, field cleared | Physical USB scanner |
| Mid-sale power loss | Partially complete | WAL + `synchronous=FULL`; sale commit is one transaction; exactly-once replay tests | Pull the plug mid-sale on a till |
| Cashier Mode / Admin Mode UI, English and Arabic (RTL), light/dark, density | Complete | Playwright English + Arabic flows, dark/compact screenshot. The unit test fails on any untranslated `t()` key or status label. Backend messages translated through `tb()`. | Visual check in WebView2 during the soak |
| Performance targets (100k products) | Complete | Numbers above (Linux sandbox) | Re-measure on the till hardware during the soak |
| Windows desktop shell (single instance, Credential Manager, ProgramData ACLs, 30-day rotating logs) | Partially complete | Built and unit-tested on Windows CI; launched under Xvfb on Linux | First launch on a Windows 10/11 till |
| NSIS installer (per-machine, firewall rules, ACLs, data kept on uninstall, embedded WebView2) | Partially complete | Built by Windows CI (above); embedded WebView2 enforced by CI | Install / upgrade / uninstall on Windows 10 and 11 |
| CI (lint, types, unit, E2E, Windows build + installer) | Complete | GitHub Actions green on this branch | — |
| Release workflow (tag → draft release, SBOMs, SHA-256 sums, optional signing) | Partially complete | Workflow lint-clean; SBOM generation run locally | First tag run |
| Code signing | Deferred | Unsigned is accepted for internal soak | Authenticode certificate |
| Auto-update (flag `updates`) | Partially complete | Ed25519-signed manifest, size + SHA-256 checks, re-verify before install, safety backup; refuses unsigned builds (`tests/updates.rs`) | Signing key (`AMWAPOS_UPDATE_PUBKEY`), hosting, install run on Windows |
| WhatsApp (flag `whatsapp`) | Partially complete | Node sidecar on 127.0.0.1 with token + lock (`sidecar/test`), supervisor states (`tests/sidecar.rs`), outbox send-once/retry, inbox, templates EN/AR (`tests/automation.rs`) | Pairing with a real phone; Baileys link never exercised against WhatsApp servers |
| Invoice scan + payment screenshot reviews (flags `ocr`, `payment_reviews`) | Partially complete | Bundled eng+ara models verified by SHA-256; real OCR in tests; parse/match/confirm → draft PO only, never stock (`tests/automation.rs`, `tests/sidecar.rs`) | Accuracy on real supplier invoices and BenefitPay screenshots |
| AI assistant (flags `ai`, `ai_mutations`) | Partially complete | Read tools as the signed-in user; proposals → confirm → normal command → undo by compensating record; fake-provider loop test (`tests/ai.rs`) | API key and owner consent; never called against the real provider here |
| Card terminal / BenefitPay integration | Deferred | Tenders recorded manually with a reference | Provider SDK, merchant account, owner, rollback |
| Migration (CSV/XLSX/ZIP/folder) | Complete | Detect → map → preview (no writes) → apply through normal commands (`tests/migration.rs`) | — |
| Customer credit (flag `customer_credit`) | Complete | Append-only ledger, limit + manager override, refunds, cash payments in drawer (`tests/credit.rs`) | — |
| Windows Hello step-up (flag `windows_hello`) | Partially complete | Runtime gate + audit (`tests/step_up.rs`); WinRT call type-checked for Windows | Run on a Windows machine with Hello |
| PDF receipts (flag `pdf_receipts`) | Complete | Raster PDF after commit; failure never affects the sale (`tests/automation.rs`) | — |
| Database encryption at rest | Deferred (known limit 3) | BitLocker guidance and threat paragraph in SECURITY.md | Owner decision |
