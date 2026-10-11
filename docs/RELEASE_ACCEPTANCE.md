# Release acceptance

This file is the single record of the release candidate (RC): what was verified automatically,
what has to be verified in the store, and who signs it off. Nothing here is claimed that was not
done. A line marked **PENDING EXTERNAL ACCEPTANCE** needs the store's computers, hardware, data
or owner.

**Classification: RELEASE CANDIDATE VERIFIED — STORE ACCEPTANCE PENDING.**
No tag, no published release, no "latest" release.

## 1. Lineage

| | |
| --- | --- |
| Wave 8 approved commit | `ab1ac07d3a198ca8aebc08b9fc2eebebb502699b` (CI run `38064849380`, all five jobs green) |
| `main` | Fast-forwarded from `94fc121` to `ab1ac07` (no force, no squash, no rewrite). Main CI run `38138883983` |
| RC branch | `claude/amwapos-release-candidate`, created from `ab1ac07` |
| RC changes | Qualification only: a disaster-recovery test (`tests/release_dr.rs`) and documentation. **No application code changed.** These commits are on the RC branch only; `main` stays at `ab1ac07` |
| RC commit and CI | Recorded in §11 |

## 2. Release identity (read from the manifests)

| Item | Value |
| --- | --- |
| Version | **Current release-candidate version: 0.1.0**, consistent in `Cargo.toml` (workspace; all four crates inherit), `Cargo.lock`, `src-tauri/tauri.conf.json`, `package.json`, `package-lock.json`, `audit::APP_VERSION`, installer name `AMWAPOS_0.1.0_x64-setup.exe`; the CI installer smoke test checks the installed version against `tauri.conf.json` |
| Product name / identifier | `AMWAPOS` / `com.amwapos.pos` (unchanged; Windows upgrade identity) |
| Installer | NSIS, per-machine, English UI, WebView2 bootstrapper embedded (CI fails otherwise) |
| Exe metadata | Tauri writes product name and version 0.1.0 from `tauri.conf.json`; build commit embedded as `AMWAPOS_BUILD_SHA` and written to the log at start-up |
| Updater metadata | None in CI builds: `AMWAPOS_UPDATE_PUBKEY` is not set, so the app never checks for or installs updates. `latest.json` is produced only by `release.yml` on a tag with `UPDATE_SIGNING_KEY` |
| Schema | 38 (migrations 1–38) |
| Sync protocol | 2 (hub and tills must run the same version; a mismatch shows "Update needed", selling continues) |
| Data | `%ProgramData%\AMWAPOS\data` (database, `backups\`, `backups\safety\`, `library\`, `expenses\`, `invoice-scans\`, `cases\`, `payment-reviews\`, `whatsapp*\`, `receipts\`, `ocr\`, `updates\`); logs in `%ProgramData%\AMWAPOS\logs` (daily, 30 kept). ACL set by the installer: SYSTEM and Administrators full, Users modify |
| Backup format | `<name>.amwbak` (SQLite online-backup copy) + `<name>.amwbak.json` manifest (SHA-256, schema, record counts) + `AMWAPOS-files/<ab>/<sha256>` for Document Library files |
| Credential store | Windows Credential Manager (`keyring`, service `AMWAPOS`): hub master secret, device key, AI keys. Per Windows account; not in backups |
| Network | Inbound TCP 47800 (firewall rule added by installer, private profile, bound to `amwapos.exe`). The hub also answers UDP 47801 discovery probes; the installer adds no rule for it and removes the old one |
| Install / upgrade / uninstall | Install and upgrade over the top keep the data folder. A safety copy `backups\pre-migration-v<N>-<time>.db` is taken before migrations. Uninstall removes the program and firewall rules and keeps `%ProgramData%\AMWAPOS` |
| Downgrade | **Not supported.** An older version refuses a newer database. See OPERATIONS.md "Upgrade and rollback" |
| Windows privileges | Administrator to install/uninstall (per-machine, firewall, ACL). Normal user to run |
| First run | Setup wizard: new standalone store, new hub, or join a hub. Optional modules (AI, WhatsApp, OCR, credit, loyalty, …) are off by default |
| External binaries | Tesseract OCR (UB Mannheim build from Chocolatey at build time, **version not pinned**; the CI log prints `tesseract --version`), OCR models `eng`/`ara` from `ocr/models`, WebView2 bootstrapper. Nothing else is downloaded at install |
| Signing | **Installer signing: UNSIGNED.** No certificate is configured; nothing is self-signed. SmartScreen will warn |

## 3. Automated verification

Local run on the RC content (Linux sandbox, Rust 1.99.0 as CI, Node 22), 2026-10-11:

| Check | Result |
| --- | --- |
| `cargo fmt --check`, `clippy --all-targets -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` | **621 passed, 0 failed, 5 ignored** (60 suites: core 521, hub 100, desktop and devserver 0) |
| `tsc -b`, `eslint`, `prettier --check src` | clean |
| vitest | 49 passed |
| `vite build` | ok |
| Playwright (real backend, English/Arabic route sweep, 1024×768) | **29 passed** (4.6 min) |
| `npm audit --omit=dev` | 0 vulnerabilities |
| `cargo audit` | 0 vulnerabilities; 10 informational warnings (unmaintained/yanked; `glib` unsound but Linux-only) |
| Migration chain, sync classification, AI policy matrix, adversarial, permission matrix, backup/restore | inside the workspace run: `migration.rs`, `wave8_cross.rs`, `sync.rs`, `wave7_sync.rs`, `ai_hardening.rs`, `wave8_ai.rs`, `ai_byok.rs`, role matrices in `wave1`–`wave8`, `release_dr.rs` |

CI results at the RC commit are in §11. Commands:

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
npm run typecheck && npm run lint && npm run format:check && npx vitest run && npx vite build
cargo build -p amwapos-devserver && npx playwright test
cargo test -p amwapos-core --release --test perf -- --ignored --nocapture
npm audit --omit=dev --audit-level=high && cargo audit
```

**Ignored tests (5), all deliberate:**

| Test | Why | Classification |
| --- | --- | --- |
| `perf::perf_100k_products` | Release-build benchmark | Run by hand for this RC (§9) |
| `perf::perf_wave8_intelligence` | Release-build benchmark | Run by hand for this RC (§9) |
| `image_live` × 3 | Calls real picture search on the internet | Live network; out of CI by design |

No `.only`, `skip`, `fixme`, focused tests, `TODO`/`FIXME` markers or disabled flows. One
Playwright test intercepts `sync.status` and `backup.health` to draw the "update needed" and
"backup overdue" states at 1024×768; everything else runs against the real backend.

## 4. Production database upgrade

**REAL PRODUCTION DATABASE UPGRADE — PENDING EXTERNAL ACCEPTANCE.** No production or acceptance
database copy was supplied to this environment (the only `amwapos.db` files present are empty
4 KB development artefacts). None was fetched from anywhere.

Automated upgrade evidence: every older schema (1, 9, 22, 27, 30, 33, 36) upgrades to 38 with
`integrity_check` ok, no foreign-key violations and nothing invented (`wave8_cross.rs`), plus
the per-wave migration tests.

Procedure for the store copy (owner present):
1. Close AMWAPOS on the hub. Copy `amwapos.db` (and `-wal`/`-shm` if present) to an encrypted
   drive. Record its SHA-256 (`Get-FileHash -Algorithm SHA256`). Keep that original untouched.
2. Copy it again to a test folder on a test PC; never commit, upload or e-mail it.
3. Before: record counts only (sales, refunds, products, stock on hand total, open payables
   total, Z close count, last receipt number). No names, phone numbers or amounts per customer.
4. Start the RC with `AMWAPOS_DATA_DIR` pointing to the test folder: the app migrates it.
5. After: Diagnostics → database ok; the same counts and totals; reprint the last Z close and
   one receipt and compare with paper; staff sign in with their own PINs; check one restricted
   role cannot open Admin.
6. Documents: Admin → Documents lists adopted expense attachments and scans; missing files are
   shown as missing, not hidden.
7. Record the result in §8 with the SHA-256 of the original (not the file).

## 5. Backup and disaster recovery

| Check | Status | Evidence |
| --- | --- | --- |
| Backup with manifest and document files; hashes verified | PASS | `release_dr.rs`; `wave8_cross.rs` |
| Restore into an **empty, separate** data folder (second PC simulation): integrity, schema 38, FKs, business, sales, sale items, payments, receipt snapshots, stock movements, products, users, documents, document pages, files, memory all byte-identical; staff sign in with their own PINs; document files intact; library and memory search work | PASS | `release_dr.rs` |
| A different business needs explicit consent before it is replaced | PASS | `release_dr.rs` |
| Restore on a real second Windows PC | PENDING EXTERNAL ACCEPTANCE | Procedure: OPERATIONS.md "Restore on a second PC" (10 steps) |
| Backups to the store's backup drive, hub left running | PENDING EXTERNAL ACCEPTANCE | OPERATIONS.md "Backup rule" |

Not in a backup (documented in OPERATIONS.md): Credential Manager secrets (hub secret, device
key, AI keys), WhatsApp session, payment-review screenshot files, WhatsApp media, saved receipt
PDFs. After a restore on another PC the hub credentials are reset and tills pair again.

Rollback: **schema downgrade is not supported**; OPERATIONS.md "Upgrade and rollback" gives the
procedure (stop, preserve, reinstall the previous version, restore the pre-upgrade backup,
verify, reconcile post-upgrade sales).

## 6. Defects found in qualification

| # | Class | Defect | Decision |
| --- | --- | --- | --- |
| 1 | P2 | The "Offline test model" AI provider ships and is selectable in Settings → AI. It is labelled as a test model, sends nothing, AI is off by default, and its proposals still need confirmation | Owner acceptance needed. Not changed in the RC (removing it changes AI defaults) |
| 2 | P2 | Backups do not include payment-review screenshots, WhatsApp media/session or Credential Manager secrets | Documented (OPERATIONS.md); owner acceptance. Changing backup scope is a feature, not an RC fix |
| 3 | P2 | Tesseract version bundled into the installer is not pinned (Chocolatey latest at build time) | Owner acceptance; the CI log records the version. Pin before a public release |
| 4 | P3 | The hub answers UDP 47801 discovery but the installer opens no rule for it; Windows may show a firewall prompt on first hub start | Check during the clean-install test; answer "private networks only" or cancel. Tills use the hub address |
| 5 | P3 | OPERATIONS.md said "three choices" and listed two; RELEASE.md named the wrong file for the version | Fixed (docs) |

No P0 or P1 defect was found. No application code was changed.

## 7. Store acceptance matrix

Status values: PASS, FAIL, PENDING EXTERNAL ACCEPTANCE, NOT APPLICABLE.

| Area | Automated evidence | Real-hardware evidence | Status | Blocker | Notes |
| --- | --- | --- | --- | --- | --- |
| Installation (clean PC) | CI installer smoke test (silent install, launch, setup, sale) | None | PENDING EXTERNAL ACCEPTANCE | Yes | Windows 10/11 store PC |
| Installer identity / version | CI smoke checks version | None | PENDING EXTERNAL ACCEPTANCE | No | Check Programs and Features shows AMWAPOS 0.1.0 |
| Upgrade over previous install | CI reinstall step | None | PENDING EXTERNAL ACCEPTANCE | Yes | Upgrade from the build currently in the store |
| Uninstall keeps data | CI uninstall step | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Real DB upgrade | Upgrade tests from schemas 1–36 | None | PENDING EXTERNAL ACCEPTANCE | Yes | §4 |
| Financial reconciliation after upgrade | Migration tests | None | PENDING EXTERNAL ACCEPTANCE | Yes | §4 step 5 |
| Backup creation | Tests | None | PASS (automated) / PENDING on store drive | Yes | Backup destination |
| Restore, same PC | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Restore, second PC | `release_dr.rs` | None | PENDING EXTERNAL ACCEPTANCE | Yes | |
| Document files in backup | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Login / PIN / roles | Tests, Playwright | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Barcode scanner | Scanner-burst Playwright | None | PENDING EXTERNAL ACCEPTANCE | Yes | |
| PLU | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Scale labels | Tests | None | PENDING EXTERNAL ACCEPTANCE | Yes | Store's scale label format |
| Cash sale / split / refund / void | Tests, Playwright | None | PENDING EXTERNAL ACCEPTANCE | No | |
| ESC/POS printer (spooler / network) | Byte-level printing tests | None | PENDING EXTERNAL ACCEPTANCE | Yes | |
| Arabic on paper | Raster tests (no `?`) | None | PENDING EXTERNAL ACCEPTANCE | Yes | |
| Cash drawer | Kick command test | None | PENDING EXTERNAL ACCEPTANCE | Yes | |
| Shift open/close, X/Z | Tests, Playwright | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Hub + tills over store Wi-Fi | Sync, encrypted channel, fake-terminal tests | None | PENDING EXTERNAL ACCEPTANCE | Yes | Soak |
| Pairing | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | No pairing codes in evidence |
| Hub/terminal disconnect, late sync | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Sync reconciliation | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Credential rotation / revocation | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | Not on the only production till |
| Full cashier day | Playwright flows | None | PENDING EXTERNAL ACCEPTANCE | Yes | |
| Trading day / EOD | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Inventory / stocktake | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Lots and expiry | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Procurement / receiving / three-way match | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Payables | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | No fake liabilities in production |
| Pricing / channels / merge | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | Do not merge real products as a test |
| Promotions / coupons / bundles | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Alert Centre / Terminal Health | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Document Library + OCR on real documents | Tests (bundled OCR in CI) | None | PENDING EXTERNAL ACCEPTANCE | Yes | Merchant's own documents stay on the PC |
| Business Memory | Tests | None | PENDING EXTERNAL ACCEPTANCE | No | |
| Cash-flow Radar vs real payables | Tests | None | PENDING EXTERNAL ACCEPTANCE | Yes | Owner review |
| AI BYOK | Tests with fake provider | None | PENDING EXTERNAL ACCEPTANCE | Yes | Owner's key; never logged or shown |
| Arabic UI | vitest coverage, Playwright Arabic sweep | None | PENDING EXTERNAL ACCEPTANCE | No | |
| 1024×768 | Playwright layout suite | None | PENDING EXTERNAL ACCEPTANCE | No | Store's screen |
| Security review | §10 / SECURITY.md | — | PASS | No | |
| Dependency audit | CI | — | PASS | No | |
| Performance | Benchmarks §9 | None | PASS (sandbox) | No | Re-measure on the store PC |
| Test-only controls absent from installer | Source review; installer smoke "no test data" | — | PASS | No | |
| Logs free of secrets | Source review; debug-level run: owner PIN, a wrong PIN and an AI key entered, then log, database and WAL scanned — none found | None | PASS (automated) / PENDING on store logs | No | |
| Installer signing | — | — | FAIL (UNSIGNED) | Yes, for public distribution | Owner decision |

## 8. External work remaining (release blockers kept visible)

1. Unsigned installer — owner decision; buy a certificate before public distribution.
2. Install on the real store PC (clean install, upgrade, uninstall).
3. Barcode scanner.
4. Scale labels.
5. ESC/POS printer.
6. Arabic on paper.
7. Cash drawer.
8. Hub plus tills over the store Wi-Fi (soak).
9. Backup destination (second drive) with the hub left running.
10. Restore on a second PC.
11. Upgrade of a real database copy (§4).
12. OCR on real supplier documents.
13. Merchant review of the Cash-flow Radar.
14. AI with the merchant's own key (BYOK), only if the merchant authorises the data flow.

Pilot status: **not started.** Plan in OPERATIONS.md "Pilot".

## 9. Performance (release build, Linux sandbox)

`cargo test -p amwapos-core --release --test perf -- --ignored --nocapture`, 2026-10-11:

| Measure | RC | Wave 8 report | Target |
| --- | --- | --- | --- |
| P95 barcode scan (100k products) | 1.42 ms | 1.29 ms | 50 ms |
| P95 product search | 9.84 ms | 5.99 ms | 150 ms |
| P95 cart mutation | 1.04 ms | 1.02 ms | 100 ms |
| P95 sale commit | 10.84 ms | 9.97 ms | 500 ms |
| P95 scale-label / PLU scan | 1.59 / 1.54 ms | — | 50 ms |
| P95 20-line sale commit with 1,000 offers | 18.94 ms | — | 500 ms |
| Library search, 50,000 documents, P95 | 243 ms (role-scoped 238 ms) | 216 ms | — |
| Memory search, 5,000 memories, P95 | 27.3 ms | 11.5 ms | — |
| Cash-flow Radar, slowest | 34 ms | 45 ms | — |

The two benchmark tests run at the same time in one process, so the shared sandbox CPU moves the
numbers between runs; every figure stays far inside its target. No application code changed
since Wave 8. Re-measure on the store PC.

## 10. Security

See SECURITY.md "Release-candidate security review".

## 11. RC record

Filled from the RC commit's own CI run.

## 12. Owner sign-off

| Item | Name | Date | Signature |
| --- | --- | --- | --- |
| Store acceptance (§7) | | | |
| Unsigned installer accepted for the pilot | | | |
| P2 defects accepted (§6) | | | |
| Go-live | | | |
