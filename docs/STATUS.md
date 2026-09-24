# Completion status

Classes:
- **VERIFIED COMPLETE:** implemented, and automated tests exercise it in this repository.
- **FUNCTIONALLY COMPLETE WITH LIMITATIONS:** works; the limits are listed.
- **PARTIALLY COMPLETE:** some of the requirement is implemented.
- **BLOCKED:** needs something outside the repository.

Evidence was re-run on 2026-09-24:
- `cargo test --workspace`: 30 + 7 + 15 + 3 + 6 + 1 tests pass; the perf test runs on demand.
- `clippy -D warnings`: clean.
- `vitest`: 12 pass.
- Playwright: 2 pass.
- The perf run (release build, 100k products) is summarised in OPERATIONS.

| Area | Status | Evidence / limitations |
| --- | --- | --- |
| Integer money, VAT incl./excl., discount allocation, rounding | VERIFIED COMPLETE | `money`, `pricing` unit tests; flow tests; vitest money tests |
| Server-authoritative cart & pricing, effective-dated prices | VERIFIED COMPLETE | flow and backoffice tests |
| Exactly-once sale / refund / cash / bulk price / import | VERIFIED COMPLETE | idempotency replay and mismatch tests; sync lost-response test |
| Append-only financial history, hash-chained audit | VERIFIED COMPLETE | trigger tests, audit verify; E2E checks "Audit chain verified" |
| PIN auth (Argon2id), lockout, roles, backend permission checks | VERIFIED COMPLETE | core tests; E2E: a cashier has no Admin |
| Manager override (single-use, permission-bound, audited) | VERIFIED COMPLETE | core tests; E2E wrong PIN, then correct PIN, then audit shows the approver |
| Shifts, blind close, variance approval, cash in/out | VERIFIED COMPLETE | flow tests; E2E expected-cash check |
| Refunds bounded by refundable qty, exact proration | VERIFIED COMPLETE | flow tests; E2E refund |
| Inventory ledger, weighted-average cost, adjustments, stocktake | VERIFIED COMPLETE | backoffice tests |
| Suppliers, purchase orders, receiving | VERIFIED COMPLETE | backoffice tests |
| Customers, deliveries | FUNCTIONALLY COMPLETE WITH LIMITATIONS | Core tests cover these. There is no dedicated E2E and no customer notification (WhatsApp is not enabled). |
| Reports (14) + CSV export | VERIFIED COMPLETE | Report tests. Exports are guarded against formula injection. |
| CSV product import | VERIFIED COMPLETE | Preview/apply tests, duplicate isolation, scientific-notation guard, 100k import |
| Backup / verified restore / safety backup / schedule | VERIFIED COMPLETE | Round-trip and tamper tests; E2E backup. On Windows, scheduled backups run only while the app is open. |
| Multi-terminal LAN sync (pairing, signing, offline queue, dead letters, hub identity, version check) | FUNCTIONALLY COMPLETE WITH LIMITATIONS | Sync convergence tests and the signed HTTP test. Traffic is authenticated but not encrypted (no TLS). Not yet tested on real store Wi-Fi hardware. |
| Printing (ESC/POS network, Windows spooler, file) | FUNCTIONALLY COMPLETE WITH LIMITATIONS | `tests/printing.rs`: receipt content and 80 mm width, COPY reprint, a failed printer keeps the sale and retry works, ESC/POS init/cut. **Arabic text prints as `?`** (no raster rendering yet). Not tested on a physical printer or drawer. |
| Barcode scanner (HID wedge) | FUNCTIONALLY COMPLETE WITH LIMITATIONS | Burst detection and scan queue are tested in vitest and E2E (typed at 5 ms/char). Physical scanner untested. |
| Cashier Mode / Admin Mode UI per visual spec | FUNCTIONALLY COMPLETE WITH LIMITATIONS | All admin sections exist. Screenshots were reviewed at 1366×768. Arabic UI (RTL) is not implemented. |
| Performance targets (100k products) | VERIFIED COMPLETE | P95 scan 0.56 ms, search 14.9 ms, cart 0.43 ms, commit 5.6 ms |
| Windows desktop shell (single instance, credential store, ProgramData, logs) | FUNCTIONALLY COMPLETE WITH LIMITATIONS | Linux build booted under Xvfb. The Windows exe cross-compiles, but has not been run on Windows here. |
| NSIS installer (per-machine, firewall rules, ACLs, data kept on uninstall) | PARTIALLY COMPLETE | Installer built by cross-compilation (`AMWAPOS_0.1.0_x64-setup.exe`) and its contents inspected. Install, upgrade and uninstall on a real Windows VM are **not yet run**. |
| CI (lint, types, tests, E2E, Windows build), release (SBOM, SHA-256, signing) | BLOCKED | The workflows are written and every step passes locally. GitHub Actions jobs on this repository end within about 3 s with no logs, so the runners are not starting: check Actions settings and billing for the account. |
| Code signing | BLOCKED | Needs an Authenticode certificate (`WINDOWS_CERT_PFX` / `WINDOWS_CERT_PASSWORD` secrets). |
| Auto-update | BLOCKED | Updates are installed from signed installers. The Tauri updater needs a signing key and a hosting URL. |
| WhatsApp notifications | BLOCKED | Needs a WhatsApp Business account and API credentials. The Admin page says it is not enabled. |
| Invoice scan (OCR) | BLOCKED | Needs an OCR provider decision and credentials. The page says it is not enabled. |
| AI assistant (read tools, preview/confirm mutations) | BLOCKED | Needs an API key and a data-sharing decision by the owner. The page says it is not enabled, and no uncontrolled mutation path exists. |
| Card terminal / BenefitPay integration | BLOCKED | Tenders are recorded manually with a reference. Integration needs a provider SDK and a merchant account. |
