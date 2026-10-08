# Operational control (Wave 7)

How AMWAPOS notices operational problems, puts them in front of the right
person once, helps them act safely, and closes the loop only when the problem
has really gone.

```
SIGNAL → DEDUPLICATE → CLASSIFY → CASE → ACKNOWLEDGE → INVESTIGATE
       → SAFE ACTION → VERIFY RECOVERY → RESOLVE → RETAIN HISTORY
```

Everything below is one source of truth per fact. There is no second alert
engine, no jobs table, no copy of device state and no secret in the database.

| Concern | The one place it lives |
|---|---|
| Something to look into | `cases` + `case_events` (Wave 2, widened) |
| What the checks remember between runs | `alert_conditions` (memory, never shown as alerts) |
| Records that could not be saved | `sync_dead_letters` (widened) |
| What a terminal reported | `device_heartbeats` (widened) |
| Open shifts | `shifts` |
| A terminal's credential | derived from the hub master secret (OS credential store) + `devices.credential_version` |
| Who did what | `audit_logs` |

## 1. Alert Centre

The Admin page **Alert Centre** (`/admin/cases`) is the cases page, extended.
A case is opened either by a person (a drawer that did not match, Wave 2) or
by the system from a condition it measured. `cases.source` says which:
`user`, `system`, or `legacy` (the old AI inbox, §2). A system case never
pretends a person opened it: `created_by` is NULL, its events have no user
and carry `"actor": "system"` in their evidence.

Lifecycle (unchanged from Wave 2, enforced by triggers):
`new → acknowledged → in_progress → resolved | dismissed`. A finished case
never changes; facts never change; nothing is deleted.

For system cases:
* `facts_json` is what was measured when the case opened; `latest_json` is
  the most recent measure; `first_seen_at`, `last_seen_at`, `occurrences` and
  `condition_active` follow the condition while the case is open.
* `link` is the screen that fixes it (Sync problems, Terminals, Backups,
  Diagnostics, Payment reviews, Deliveries). The drawer offers "Open the
  screen that fixes this".
* A person finishes it with an outcome from a fixed list: `fixed`,
  `not_a_problem`, `duplicate`, `other` (cash cases keep their own list).
  `recovered` is not on the list: only the system records it.

The Dashboard's "Needs attention" shows the top five open cases (by severity,
then age) and a count of the rest. It no longer computes its own sync, backup,
printing or cash alerts. The opening checklist says how many cases still need
attention and links to the Alert Centre.

### Checks

`ops::measure` (read-only, bounded, grouped queries) runs every minute on the
hub (or a standalone store) inside one write transaction with
`ops::reconcile`. A person with `cases.manage` can run it at once ("Check
now"). Terminals never run it and never write cases.

| Kind | Condition (from evidence only) | Severity | Fix screen |
|---|---|---|---|
| `sync_failures` | open records in `sync_dead_letters`, one case per sending computer **and reason** | high if any money/stock record, else medium | Sync problems |
| `terminal_not_seen` | an active terminal not seen for 15 min **while it has an open shift** | high after 120 min | Terminals |
| `terminal_backlog` | oldest waiting change older than 15 min (or, for terminals that do not report it, 50+ waiting and nothing sent for 15 min), while still seen | medium | Terminals |
| `terminal_incompatible` | reported sync protocol, else database version, differs from the hub's | high | Terminals |
| `credential_rotation_stale` | a staged credential rotation not picked up in 24 h | medium | Terminals |
| `backup_overdue` | the backup subsystem's own state is warning/error | medium/high | Backups |
| `print_failures` | print jobs failed in the last 24 h on this computer, the latest failure after the latest success | low | Diagnostics |
| `payment_review_backlog` | payment screenshots undecided for over an hour | medium | Payment reviews |
| `rider_cash_held` | delivery cash held by a rider for over a day, per rider | medium | Deliveries |

## 2. Legacy alert migration

Migration 0033 copies every `ai_alerts` row into a `legacy` case with its own
title, time, details and dismissal (`legacy_dismissed`, by the same person, at
the same time), plus a `created` event and, if dismissed, a status event. A
count check stops the migration if any row is not copied. No case is made up
for anything that happened before the upgrade.

`ai_alerts` stays, read-only (insert/update/delete triggers), for one release.
The old B3 inbox writer (`ai_anomaly_scan`) writes nothing; dismissing in the
old inbox is refused with "This alert is now a case in the Alert Centre."

## 3. Alert sources (audit)

| Source before Wave 7 | Decision |
|---|---|
| AI inbox `hub_lag` (tills silent) | durable case: `terminal_not_seen` (only during an open shift) |
| AI inbox `backup_overdue` | durable case: `backup_overdue` |
| AI inbox `refund_spike`, `discount_spike` | dynamic metric: today's refunds/discounts on the Dashboard; not an incident |
| AI inbox / Dashboard `negative_stock` | dynamic metric: Dashboard count with a link to Inventory |
| Dashboard `sync` (dead letters) | durable case: `sync_failures` |
| Dashboard `backup` | durable case: `backup_overdue` |
| Dashboard `printing` | durable case: `print_failures` |
| Dashboard `cash` (drawer differences) | existing case: `cash_variance` (Wave 2) |
| Dashboard `payments` (screenshots waiting) | durable case after 60 min: `payment_review_backlog`; the live count stays on the Dashboard |
| Dashboard `low_stock`, `shortages`, `suggested_orders`, `unknown_barcodes`, `orders` | dynamic work queues: stay on the Dashboard; not incidents |
| Dashboard `payables`, `payables_post`, `po_approval`, `requisitions*`, `returns_credit` | approval/work queues of their own modules; unsuitable as cases |
| Rider cash custody (Wave 3 send loop) | durable case after 24 h: `rider_cash_held` |
| Credential rotation | durable case after 24 h: `credential_rotation_stale` |

## 4. Noise control

* **One incident, one case.** Every system condition has a stable
  `dedupe_key` (e.g. `sync_failures:<device>:<reason>`); a partial unique
  index allows one open case per key. 400 refused records from one till for
  one reason are one case.
* **Debounce.** A kind can wait before opening a case (`raise_after_min`,
  e.g. a backlog must persist 10 minutes).
* **Hysteresis.** A condition must stay clear for `clear_after_min` before
  the case resolves itself (e.g. 5 minutes for "till not seen"), so a
  flapping till does not open and close cases. While open, a condition that
  returns adds an occurrence and an event, not a new case.
* **Recurrence is a new episode.** After a case finished, the same condition
  returning later opens a new case (`facts.episode` counts them).
* **Dismissed ≠ resolved.** A person who dismisses or resolves a system case
  while the condition still holds is respected: the checks do not reopen it
  until the condition has cleared once (`alert_conditions.suppressed`).
* **Deterministic severity** from the facts (money or stock records, minutes
  silent), never from a model.
* **Auto-resolve only on proven recovery.** The system resolves a case only
  after it measured the condition gone (and stayed gone for the hysteresis),
  with resolution `recovered`, no user, a system event and a
  `case.auto_resolved` audit record.

## 5. Sync Reconciliation Centre

`sync_dead_letters` (migration 0034, copy-checked) gains `reason_code`,
`retryable`, a third state `closed`, and `resolution`, `resolution_note`,
`resolved_by`, `resolved_at`. Rows from before the upgrade keep their text,
become `legacy_unclassified` (retryable) and keep a NULL resolution.

Reason codes are decided from the error's code and the apply path's own
messages when the record is refused:

| Code | Plain words | Retry can help |
|---|---|---|
| `missing_dependency` | Waiting for a related record | yes |
| `version_mismatch` | App versions differ | yes, after updating |
| `storage_error` | Database busy or full | yes |
| `unknown`, `legacy_unclassified` | Unexpected / not recorded | yes (safe) |
| `not_permitted` | Not allowed from this till | no |
| `invalid_record` | Incomplete record | no |
| `conflict` | Conflicts with a saved record | no |
| `superseded` | A newer version was saved | no |

A child record whose parent has not arrived (a sale line before its sale) is
`missing_dependency`, not a refusal.

**Actions (hub or standalone, `sync.manage`):**
* **Try again** runs the stored change through the normal apply path
  (`apply_change` with the sending terminal's ownership checks). A finished
  record reports `already_finished` and is never applied twice; operation ids
  replay. A shared record with a newer `updated_at` on the hub is not
  overwritten: it becomes `superseded`.
* **Try everything shown again / the selected**: eligible rows only (open,
  retryable, refused on this hub), at most 500 per request; the answer counts
  eligible, ineligible, saved, still failing and superseded; one audit record.
* **Close without applying**: a reason (5–500 characters); a money or stock
  record also needs an explicit confirmation. The row is kept as `closed`
  with who, when and why. The audit holds the record kind and reason, never
  the record.
* There is no way to edit a stored record. Identity and finished rows are
  fixed by triggers; nothing is deleted.

**Settling by itself:** a record refused earlier and accepted on a later send
settles (`recovered`) on the hub and on the terminal; a terminal retries hub
changes it could not save (retryable reasons, at most 50 per pull, each at
most every five minutes); a terminal's heartbeat lists its refused records
and the hub answers which it has since saved or closed (`settled_on_hub`).

Terminals show their own list read-only ("Sync problems are handled on the
hub"). Each person's action is added to the open `sync_failures` case as
evidence under their name.

## 6. Terminal Health

`device_heartbeats` (migration 0035) gains `protocol_version`,
`oldest_pending_at`, `last_sync_ok_at`, `problem_count`, `last_heartbeat_at`.
They stay NULL until a terminal that knows them reports them; older terminals
leave them out.

**Terminals** (`/admin/terminals`, `devices.manage` or `sync.manage`) shows,
per terminal: health, connection, last seen, the app, database and sync
protocol versions separately (each with whether it matches the hub),
waiting records and the oldest one, records the hub refused (link to Sync
problems), the signed-in user, the open shift **from the shifts table**, open
cases, and the credential state. Beside it, store facts: backup state and
this computer's print failures (terminals do not report printers, so theirs
are shown as unknown).

`terminal_health::assess` (pure, unit tested) decides:

| Health | When |
|---|---|
| `revoked` | the device is revoked |
| `unknown` | never seen, or seen but never sent its own report |
| `attention` | not seen ≥ 5 min **during an open shift**, protocol or database mismatch, backlog, refused records, or last sync failed |
| `offline` | not seen ≥ 5 min with no open shift (switched off after closing) |
| `healthy` | reported, seen, and none of the above |

Facts a terminal never reported are listed as unknown, never filled in.

## 7. Credential rotation

A terminal's credential is `HMAC(master, "device:<id>")` for version 1 (the
pre-Wave 7 derivation, so every existing terminal keeps working unchanged)
and `HMAC(master, "device:<id>:v<n>")` for later versions. Only versions and
times are stored (migration 0036, hub-local columns never replicated).

Staged handshake, safe for a terminal that is offline for days:
1. A person with `devices.manage` starts a rotation on the hub. The current
   credential keeps working; version n+1 is staged.
2. The terminal's next heartbeat (signed and sealed with its current
   credential) receives the new credential inside the encrypted reply.
3. The terminal stores it in its own credential store and signs with it from
   then on, keeping the previous one until the hub confirms.
4. The first request signed with n+1 is the proof: the hub makes it current
   (a conditional update, so concurrent requests rotate once; audited as a
   system action). The previous version is accepted for **10 minutes**
   (requests already on their way), then never. The hub seals each reply with
   the credential that signed the request.
5. The next heartbeat confirms the version; the terminal forgets the old one.

If the rotation is cancelled before the proof, the hub refuses the new
credential and the terminal goes back to the one it kept — only on a
signature refusal, never on a clock or replay error, so a completed rotation
can never be undone by accident. A rotation not picked up in 24 hours opens
`credential_rotation_stale`, which resolves itself when it is picked up.

There are never two permanent credentials and no global fallback secret. No
credential is written to the database, a log, an audit record, a case, a
diagnostic, an API answer other than the sealed heartbeat reply, or the
assistant (tests scan the database files for the keys).

## 8. Device revocation

Revoking is separate from rotating (`devices.revoke`, `devices.manage`, a
reason). A revoked terminal is refused at once. **Lost or stolen** also moves
its credential version on and blocks re-activation, so the old credential
can never work again; a found device is paired as a new terminal. Pausing a
device (`devices.set_active`) is unchanged and reversible.

**Reset every terminal** (a new master secret) is owner-only and needs the
typed confirmation `RESET ALL TERMINALS`; every terminal must be paired again.

## 9. Durable jobs decision

Not built. The only background work in this wave is the one-minute check on
the hub: it is idempotent (unique open key per incident, conditional updates)
and stateless between runs except for `alert_conditions`, so a missed or
repeated run changes nothing but timing. Retries are person-initiated and
idempotent per record; terminal-side automatic retries are bounded per pull.
A generic jobs table would add a second source of truth (job state vs record
state) without a failure mode it fixes. Revisit if a future wave needs
multi-step work that must survive restarts mid-way (e.g. external API
pushes).

## 10. Permissions

| Action | Permission | Where |
|---|---|---|
| See the Alert Centre | `cases.view` | any |
| Acknowledge, note, assign, run checks | `cases.manage` | hub / standalone |
| Resolve, dismiss | `cases.resolve` | hub / standalone |
| See sync problems | `sync.manage` | any (terminal: read-only) |
| Retry, bulk retry, close without applying | `sync.manage` | hub / standalone |
| See terminal health | `devices.manage` or `sync.manage` | hub |
| Rotate, cancel, revoke | `devices.manage` | hub |
| Reset every terminal | owner + `sync.manage` + typed confirmation | hub |

Cashiers have none of these. Terminals never perform operational-control
writes. The assistant can read the Alert Centre, sync problems and terminal
health; it cannot acknowledge, resolve, dismiss, retry, close, rotate,
revoke, re-activate or mark anything healthy (each is listed as forbidden in
`ai_tools::NO_TOOL`; the earlier "propose sync retry" and "propose device
active" tools were removed).

## 11. Offline behaviour

Selling never waits for any of this. A terminal offline for days keeps its
credential, keeps its refused records read-only, retries hub changes it could
not save when it is back, learns which refused records the hub settled, and
picks up a staged credential at its first exchange. The hub keeps unknown
facts unknown while it cannot see a terminal.

## 12. Thresholds

| Name | Value | Constant |
|---|---|---|
| Not seen recently (health) | 5 min | `ops::NOT_SEEN_HEALTH_MIN` |
| Not seen during a shift (case) | 15 min; high after 120 | `NOT_SEEN_CASE_MIN`, `NOT_SEEN_HIGH_MIN` |
| Backlog | oldest waiting > 15 min, or 50+ and nothing sent for 15 min | `BACKLOG_STALE_MIN`, `BACKLOG_COUNT` |
| Payment screenshots waiting | 60 min | `PAYMENT_REVIEW_WAIT_MIN` |
| Rider cash held | 24 h | `RIDER_CASH_HOURS` |
| Old credential grace | 10 min | `device_credentials::GRACE_MINUTES` |
| Rotation not picked up | 24 h | `device_credentials::STALE_HOURS` |
| Bulk retry per request | 500 | `sync_recon::BULK_LIMIT` |

## 13. Performance

Release build, Linux sandbox, on top of the 100,000-product benchmark (`perf.rs`), with 40
terminals, 10,000 earlier cases (30% open) and 10,000 refused records:

| Operation | Time |
|---|---|
| Minute check, first run (opens 200 cases: 40 tills × 5 reasons) | 46 ms |
| Minute check, steady state | 19 ms |
| Alert Centre, first page of 3,201 needing attention | 4.0 ms |
| Alert Centre, page at offset 9,900 | 28.4 ms |
| Sync problems, first page of 7,497 waiting | 12.7 ms |
| Sync problems, page at offset 7,400 | 25.3 ms |
| Sync problems, one till and reason | 6.1 ms |
| Terminals (40) | 7.2 ms |
| Dashboard with open cases | 150.7 ms |
| Bulk retry of 500 records | 31 ms |

The POS path is unchanged (P95 scan 1.40 ms, search 5.67 ms, cart change 1.06 ms, sale commit
10.04 ms). Every list is paged (at most 200 rows per request), every count and group is one
indexed query, and the check never runs one query per row.

