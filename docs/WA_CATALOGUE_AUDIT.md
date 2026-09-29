# WhatsApp Business catalogue: forensic audit and hardening, 2026-09-29

This covers the catalogue sync that already existed (STATUS.md, "WhatsApp Business catalogue, 2026-09-28"). It was not rebuilt. It still uses the same WhatsApp link, session, supervisor, hub runtime and `whatsapp-rust` protocol code as receipts. There is no Meta Graph or Cloud API, no other provider, and no second login.

**How this was checked.** Repository inspection, deterministic core tests, protocol golden fixtures, the fake adapter with failures injected, and runtime-level integration tests. A live WhatsApp Business account was not available and was not used (see "Not verified" below).

## 1. What exists

| Layer | File | Role |
|---|---|---|
| Durable state, decisions | `crates/amwapos-core/src/wa_catalog.rs` | Mappings, fingerprints, eligibility, scan, claim/complete, runs, overview, permissions |
| Schema | `migrations/0021_wa_catalog.sql`, `0024_wa_catalog_hardening.sql` | `wa_catalog_products`, `wa_catalog_collections`, `wa_catalog_runs` |
| Worker | `crates/amwapos-hub/src/whatsapp/catalog.rs` | Capability detection, remote index, adoption, one write at a time, run remote check |
| Protocol | `crates/amwapos-hub/src/whatsapp/catalog_proto.rs` | `w:biz:catalog` stanzas: `product_catalog_add/edit/delete`, `product_catalog` read |
| Adapter | `rust_adapter.rs` (real), `fake.rs` (tests) | The same `AdapterSession` the message worker uses |
| Service | `service.rs` | Catalogue state, pokes, pace (1.2 s default) |
| Commands | `runtime.rs` | `whatsapp.catalog_status / _sync / _retry / _configure / _recheck / _product` |
| UI | `src/screens/admin/waCatalog.tsx` | WhatsApp → Catalogue page, product-editor line |

## 2. Invariants (each has a test)

- **Connection.** Every remote call uses the session the service already runs. No second client is created.
- **Account isolation.** Mappings are keyed by (linked number, product). Each job is checked against the number linked *now* before any remote call. A→B→A never mixes remote ids.
- **Idempotency.** An unchanged fingerprint means no remote write. Before an unmapped create, the catalogue is consulted and an unowned copy with the same product code is adopted. If the catalogue cannot be read, nothing is created.
- **POS authority.** Data flows POS → WhatsApp only. Nothing read from WhatsApp changes POS data.
- **Ownership.** Only products AMWAPOS created or adopted are updated, hidden or deleted. Merchant-made products are untouched.
- **Price.** Integer thousandths, with BHD fils mapped 1:1. Zero, negative and over-large prices are never sent. A hidden product keeps its last published price.
- **Image.** Only POS-stored JPEG bytes are sent, and no picture search is triggered. A missing or refused picture never blocks the product.
- **Failure isolation.** One product's failure never stops the others. Failed rows are ordered so they cannot starve the queue.
- **Retry.** Temporary errors retry at 1, 5, 30 and 120 minutes (server back-off is honoured, capped at 24 h). The fifth failure becomes `failed`. Permanent errors do not loop.
- **Terminal states.** `synced`, `hidden`, `removed`, `failed` and `remote_missing` change only on a POS change, Sync or Retry.
- **Permissions.** Writing needs both `whatsapp.manage` and `products.manage`. Viewing the status needs `whatsapp.manage`.
- **Reconnect.** A disconnect leaves work queued. Claims left by a dead worker are released at once. Reconnecting resumes without duplicates.
- **Full sync.** A full sync is a tracked run with progress. It reads the remote catalogue once per run and republishes products deleted on WhatsApp. Only an explicit sync does that.
- **Migration.** The upgrade adds columns and a table, and publishes nothing.

## 3. State machine

```
             scan / Sync / Retry
 (none) ───────────────► queued ──claim──► syncing ──Published──► synced / hidden
                           ▲  ▲                │  ├─Deleted────► removed
       edit during write ──┘  │                │  ├─Failed─────► failed        (Retry / POS change → queued)
       (fingerprint moved)    │                │  ├─RemoteMissing, run open ──► queued (remote id dropped)
                              │                │  ├─RemoteMissing, no run ───► remote_missing (Sync / Retry → queued)
                              └──Transient─────┘  (next_at = back-off; 5th → failed)
 worker start: every syncing → queued (the claims of a dead worker)
 claim older than 10 min → reclaimable
```

A run moves `open → remote check (verify pending → done | skipped) → finished`. It finishes once every row it tagged is counted. There is at most one open run per account (a unique index enforces this), so pressing Sync repeatedly joins the open run.

## 4. Findings and fixes

| # | Finding | Kind | Fix | Test |
|---|---|---|---|---|
| F1 | A product pointing at a picture whose bytes are missing was recorded *with* the picture while published without it, so the picture was never sent later | bug | `image_hash` is taken only when the bytes exist (join on `product_images`) | core `a_picture_that_is_missing_or_refused…` |
| F2 | A corrupt or refused picture failed the whole product permanently | bug | Non-JPEG or missing bytes, or a permanent upload refusal, publish without the picture and set `image_failed_hash`; a new picture is tried again | core (same) + hub `a_refused_picture_publishes…` |
| F3 | An edit made while the product's write was in flight was recorded as `synced` | race | On completion, if the desired fingerprint ≠ the sent one, the row is re-queued (auto-sync or open run) or shown as out of date | core `an_edit_made_while_a_write_is_in_flight…` |
| F4 | After a relink mid-pass, jobs claimed for number A ran on B's session | race | Each job is checked against the linked number; on a mismatch it is a temporary outcome with no remote call | hub `relinking_mid_pass_and_a_then_b_then_a…` |
| F5 | After a crash or task restart, claimed rows stayed `syncing` for 10 min | defect | The worker releases all claims at start | core `a_restarted_worker_releases…` |
| F6 | With two or more unowned copies carrying the product code, a further copy was created | duplicate | The oldest (smallest id) unowned copy is adopted; the others are left alone | hub `duplicates_are_adopted_once…` |
| F7 | The whole remote catalogue was re-read for every 5-product batch | rate | A remote index is kept in step with the worker's own writes and re-read after 10 min, a failed create, "not found", a relink, or a run's check | hub `a_large_first_sync…` (≤ 4 list calls for 60 products) |
| F8 | An "unavailable" capability was re-checked every 5 s | rate | Re-checked once a minute (or on "Check again") | hub `an_unreachable_capability_check…` |
| F9 | A product deleted on WhatsApp was noticed only if edited; full sync skipped `remote_missing` | gap | Each run reads the remote catalogue once; mapped products not listed are re-written, and a "not found" inside a run republishes them. Automatic sync still never republishes | core `a_full_sync_is_a_run…`, `automatic_sync_never_recreates…`; hub `duplicates_are…` |
| F10 | Hiding a product that lost its price sent no price | risk | The last published price is stored and used for hiding | core `hiding_a_product_that_lost_its_price…` |
| F11 | Control characters, newlines and repeated spaces in names went out raw | hardening | `clean_line` / `clean_block`; long product codes get a stable bounded retailer id | core `names_and_descriptions_are_cleaned…` |
| F12 | Status used the capability's (possibly previous) account | stale | Status and the product line use the linked number; they show "checking" until it is checked | runtime (hub relink test) |
| F13 | Sync on a terminal said "not available right now" | UX | It now says to open the page on the hub computer | code + UI text |
| F14 | No progress, retrying, out-of-date count or run history | UX | Runs table plus overview `run`, `last_run`, `retrying`, `out_of_date`, friendly errors | core run test, vitest `waCatalog.test.ts` |
| F15 | After an upload succeeded and the write failed, each retry re-uploaded the picture | waste | The uploaded URL is kept for 24 h and reused | core `an_uploaded_picture_is_reused…` |
| F16 | A "not found" left the deleted id in the cached index, so a full sync could re-adopt the deleted copy and loop (adopt → not found → requeue) | bug (found by the new tests) | "Not found" drops the index, and the run's remote check refreshes it | hub `duplicates_are…` (hangs with both safeguards removed) |
| F17 | A remote id over 64 characters was truncated to 64 and accepted, so a later edit or delete would target another id | bug (found by fixtures) | Over-long ids are refused | `catalog_proto` `untrusted_replies_are_bounded…` |

## 5. Request coverage

- **Account scoping, A→B→A.** Core `account_a_then_b_then_a…`; hub relink test.
- **Duplicates.**
  - Crash after create: hub `failures_are_isolated…`, where the reply is lost and the catalogue is unreadable, so the product waits and is then adopted.
  - Deleted mapping: hub `duplicates_are…`.
  - Rename during recovery: adoption is by product code, never by name.
  - Upload succeeds then create fails: F15.
  - Rapid Sync presses: hub `rapid_sync_presses…`, which gives one run and 5 creates.
- **Identity.** The retailer id is the product code (SKU). A code over 100 characters is shortened with a stable hash suffix.
- **Prices.** Core `exact_prices_from_one_fils_up` and proto `golden_prices…` cover 0.001, 0.010, 0.100, 0.999, 1.000, 1.250, 9.990, 10.005, 99.999, 100.000, the maximum, zero, negative and overflow.
- **Delete, archive, remote missing, unmanaged products.**
  - Archiving hides the product.
  - A product gone from the POS deletes only its own remote copy.
  - A product deleted on WhatsApp is reported, and republished only by Sync or Retry.
  - The merchant's own products are asserted untouched in every hub test.
- **Collections.** Not written: there is no write stanza in the protocol used, so none was invented. Categories are recorded as `unsupported`.
- **Full-sync stress, rate limiting, receipt responsiveness, existing WhatsApp regression.** Hub `a_large_first_sync…` runs 60 products against a WhatsApp answering in 40 ms per call:
  - a receipt goes out before the catalogue is done;
  - a customer message is received into the inbox during the sync;
  - reads and capability checks stay bounded;
  - `tests/whatsapp.rs` and `tests/whatsapp_orders.rs` still pass.
- **Product Images.** Only stored bytes are sent. The catalogue never calls image discovery.
- **Permissions.** Owner, cashier, WhatsApp-only and products-only are checked in core and through the runtime.
- **Terminal.** The worker never runs on a terminal. Commands there answer with the hub message.
- **Migrations.** `an_upgrade_publishes_nothing`. A unique index keeps at most one open run per account.
- **Operator UX.** The Catalogue page now has:
  - a first-sync explainer: what is published, what is left out and why, where pictures come from, that nothing is duplicated or touched, that collections are unsupported, and an estimated time;
  - a progress bar reading "47 / 182 products processed", with published, already up to date, hidden, removed and failed counts;
  - the remote-check state, and the last full sync;
  - retrying, out-of-date and deleted-on-WhatsApp counts;
  - friendly errors, with WhatsApp's own text on hover;
  - an explanation of the auto-sync setting;
  - Sync disabled while a run is in progress;
  - Arabic for all of it.

## 6. Not verified, and residual risks

- **No live WhatsApp Business run.** Stanza shapes follow WhatsApp Web / Baileys and are pinned by golden fixtures, not by a live server. The first real run should use a small catalogue. Check that:
  - the business account is detected;
  - one product publishes with a picture and a BHD price;
  - an edit updates the same product;
  - a second Sync writes nothing.
- WhatsApp's real limits (name and description length, picture rules, rate) are not published. The limits used are conservative choices.
- The remote check lists the catalogue in pages (at most 40 × 50 products). Past that, the check is skipped and the run says so.
