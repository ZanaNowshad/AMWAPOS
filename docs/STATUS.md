# Completion status

**Overall: engineering-complete in the sandbox, not release-complete.** No row is "done" for
release until the Windows hardware pass in [OPERATIONS.md](OPERATIONS.md) has been run, and the
**Pending** column is cleared.

Classes:
- **VERIFIED COMPLETE:** implemented, tested automatically in this repository, and with no
  dependency on Windows hardware or third-party accounts.
- **FUNCTIONALLY COMPLETE WITH LIMITATIONS:** works as tested; the listed limits remain.
- **PARTIALLY COMPLETE:** the code exists and is tested off-target, but a required verification or
  part is missing.
- **BLOCKED:** needs something outside the repository.

Evidence was re-run in the Linux sandbox on 2026-09-24:
- `cargo test --workspace`: 32 core unit + 7 back-office + 15 flow + 3 printing + 6 sync + 3
  encrypted-channel + 1 HTTP sync = 67 pass; the perf test runs on demand.
- `clippy -D warnings`: clean.
- vitest: 12 pass.
- Playwright: 2 pass.

## Known limits (open, not nice-to-have)

These stay open until fixed. The wording is frozen.

1. **Arabic receipt glyphs print as `?`** (image fallback not built).
2. **No RTL shell.**
3. **Database file unencrypted** (depends on BitLocker; see SECURITY.md).
4. **Unsigned installer; not run on Windows.**

## Matrix

| Area | Status | Evidence | Pending |
| --- | --- | --- | --- |
| Integer money, VAT incl./excl., discount allocation, rounding | VERIFIED COMPLETE | `money`, `pricing` unit tests; flow tests; vitest money tests | — |
| Server-authoritative cart & pricing, effective-dated prices | VERIFIED COMPLETE | flow and back-office tests | — |
| Exactly-once sale / refund / cash / bulk price / import | VERIFIED COMPLETE | idempotency replay and mismatch tests; sync lost-response test | — |
| Append-only financial history, hash-chained audit | VERIFIED COMPLETE | trigger tests, audit verify; E2E "Audit chain verified" | — |
| PIN auth (Argon2id), lockout, roles, backend permission checks | VERIFIED COMPLETE | core tests; E2E: a cashier has no Admin | — |
| Manager override (single-use, permission-bound, audited) | VERIFIED COMPLETE | core tests; E2E wrong PIN, then correct PIN, then audit shows the approver | — |
| Shifts, blind close, variance approval, cash in/out | VERIFIED COMPLETE | flow tests; E2E expected-cash check | — |
| Refunds bounded by refundable qty, exact proration | VERIFIED COMPLETE | flow tests; E2E refund | — |
| Inventory ledger, weighted-average cost, adjustments, stocktake | VERIFIED COMPLETE | back-office tests | — |
| Suppliers, purchase orders, receiving | VERIFIED COMPLETE | back-office tests | — |
| Reports (14) + CSV export (formula-injection safe) | VERIFIED COMPLETE | report tests; CSV escape round-trip test | — |
| CSV product import | VERIFIED COMPLETE | preview/apply tests, duplicate isolation, scientific-notation guard, 100k import | — |
| Customers, deliveries | FUNCTIONALLY COMPLETE WITH LIMITATIONS | core tests; no dedicated E2E | Customer notification needs WhatsApp (BLOCKED below) |
| Backup / verified restore / safety backup | FUNCTIONALLY COMPLETE WITH LIMITATIONS | round-trip and tamper tests; E2E backup | Backups run only while the app is running (operational rule in OPERATIONS.md). Not yet run against Windows paths, USB drives or network shares. |
| Multi-terminal LAN sync (encrypted protocol 2, SPAKE2 pairing, offline queue, dead letters, hub identity, version check) | PARTIALLY COMPLETE | sync convergence tests; encrypted HTTP tests (no sensitive bytes on the wire, protocol 1 refused, code burned after 5 wrong tries) | Windows hardware pass: firewall rules, Credential Manager, two tills + hub on store Wi-Fi, unplugging the hub |
| Printing (ESC/POS network, Windows spooler, file) | PARTIALLY COMPLETE | `tests/printing.rs`: receipt content and 80 mm width, COPY reprint, a failed printer keeps the sale and retry works, ESC/POS init/cut | Physical 80 mm printer and cash drawer; Windows spooler path never run. Known limit 1 (Arabic). |
| Barcode scanner (HID wedge) | PARTIALLY COMPLETE | burst detection and scan queue: vitest + E2E typing at 5 ms/char | Physical USB scanner at full speed |
| Cashier Mode / Admin Mode UI per visual spec | FUNCTIONALLY COMPLETE WITH LIMITATIONS | all admin sections exist; Playwright in Chromium; screenshots at 1366×768 | Rendering in WebView2 on Windows and DPI scaling not checked. Known limit 2 (no RTL). |
| Performance targets (100k products) | FUNCTIONALLY COMPLETE WITH LIMITATIONS | P95 scan 0.56 ms, search 14.9 ms, cart 0.43 ms, commit 5.6 ms, measured on the Linux sandbox | Re-measure on the actual till hardware |
| Windows desktop shell (single instance, credential store, ProgramData, logs) | PARTIALLY COMPLETE | Linux build booted under Xvfb; the Windows exe cross-compiles (mingw) | Never run on Windows |
| NSIS installer (per-machine, firewall rules, ACLs, data kept on uninstall) | PARTIALLY COMPLETE | Verification build from Linux (mingw, WebView2 downloaded at install time); contents inspected | Known limit 4. The release artifact must come from the Windows CI job with the embedded WebView2 bootstrapper (CI checks the config). Install, upgrade and uninstall on Win10/11 not yet run. |
| CI and release workflows | BLOCKED (account side) | Workflows written; every step passes locally | GitHub Actions runners not starting on this repository; the owner is fixing Actions and billing |
| Code signing | BLOCKED (deferred) | Not enabled. Unsigned builds are for internal soak only. | Authenticode certificate |
| Auto-update | BLOCKED (deferred) | Not enabled. The Updates page says updates come from signed installers. | Updater signing key and hosting |
| WhatsApp notifications | BLOCKED (deferred) | Not enabled; the Admin page says so; no fake success path | Business account, key, owner, rollback plan |
| Invoice scan (OCR) | BLOCKED (deferred) | Not enabled; the Admin page says so | Provider choice, key, owner, rollback plan |
| AI assistant | BLOCKED (deferred) | Not enabled; the Admin page says so; no mutation path exists | API key, data-sharing decision, owner, rollback plan |
| Card terminal / BenefitPay integration | BLOCKED (deferred) | Tenders are recorded manually with a reference | Provider SDK, merchant account, owner, rollback plan |
