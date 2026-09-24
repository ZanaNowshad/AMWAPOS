# AMWAPOS

Offline-first retail point of sale for Windows 10/11 (x64), built for Bahrain and GCC shops.
English and Arabic (right-to-left) user interface; Arabic prints correctly on thermal receipts.
Money is BHD in integer fils (3 decimals); VAT, discounts, refunds and cash are computed exactly
on the backend. Every till keeps selling with no network; several tills can sync through one
store hub on the local network.

**Stack:** Tauri 2 · Rust · SQLite (WAL) · React 19 · TypeScript.

## Repository layout

| Path | What it is |
| --- | --- |
| `crates/amwapos-core` | Domain logic and storage: pricing, sales, refunds, shifts, inventory, purchasing, customers, reports, import, backup/restore, audit chain, auth, sync. Migrations in `src/migrations`. |
| `crates/amwapos-hub` | LAN hub HTTP server (axum), signed sync client, UDP discovery, background runtime (sync loop, scheduled backups). |
| `crates/amwapos-devserver` | Loopback-only HTTP bridge serving the UI plus `POST /rpc` for browser development and E2E. Never shipped. |
| `src-tauri` | Desktop shell: one `rpc` IPC command, single instance, Windows Credential Manager secrets, JSON logs, NSIS installer. |
| `src` | React UI: Cashier Mode (`screens/pos`) and Admin Mode (`screens/admin`). |
| `e2e` | Playwright tests that drive the real UI against the real backend. |
| `src/i18n` | UI language: `t()` / `tb()` and the Arabic dictionary (`ar.ts`). |
| `scripts` | i18n maintenance: find untranslated strings (`i18n-rendered.mjs`, `i18n-leftovers.mjs`), extract backend messages (`rust-ui-strings.mjs`), merge translations (`i18n-add.mjs`). |
| `docs` | Architecture, security, operations, release and completion status. |

## Develop

Requirements: Node 22+, Rust stable. On Linux, the WebKitGTK dev packages are needed for `src-tauri`.

```sh
npm ci
npm run devserver          # backend on http://127.0.0.1:8787 (data in .amwapos-dev/)
npm run dev                # UI on http://localhost:1420, proxies /rpc to the devserver
npm run tauri dev          # or: the real desktop shell
```

## Verify

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                                   # unit + integration + sync + HTTP hub
cargo test -p amwapos-core --release --test perf -- --ignored --nocapture   # 100k-product benchmark
npm run typecheck && npm run lint && npm run format:check && npm test
npx vite build && npx playwright test                    # end-to-end (English + Arabic)
cargo run -p amwapos-core --example raster_preview -- out.pbm   # look at Arabic receipt rendering
```

## Build the Windows installer

On Windows: `npx tauri build --bundles nsis` → `target/release/bundle/nsis/AMWAPOS_<ver>_x64-setup.exe`.
See [docs/RELEASE.md](docs/RELEASE.md) for signing, SBOM and checksums.

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Security](docs/SECURITY.md)
- [Operations & acceptance](docs/OPERATIONS.md)
- [Release](docs/RELEASE.md)
- [Completion status](docs/STATUS.md): what is verified, what has limits, what is blocked
