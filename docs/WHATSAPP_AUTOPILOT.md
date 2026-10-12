# WhatsApp autopilot and order sync

Customers order, confirm, follow and cancel an order entirely over WhatsApp,
with no person typing replies. Every order and delivery change is sent, in
order, to the other systems the shop already runs.

The autopilot is built on the existing pieces and does not replace them:

* the WhatsApp connection (`crates/amwapos-hub/src/whatsapp/`);
* the order pipeline, which turns messages into drafts with real products,
  POS prices, stock and delivery fees (`waorders`,
  [DOCUMENTS_AND_ORDERS.md](DOCUMENTS_AND_ORDERS.md));
* the tickets, deliveries and riders already used at the till.

What is new:

| Part | Where |
| --- | --- |
| WAHA gateway adapter (an alternative to the built-in connection) | `crates/amwapos-hub/src/whatsapp/waha.rs` |
| Automatic replies, confirmation, cancellation, status, handoff | `crates/amwapos-core/src/waorders/autopilot.rs` |
| Order event feed: one row per state change, written by database triggers | migration `0039_order_events_autopilot.sql` |
| Sync targets (signed webhook, CSV file) with retries and parking | `crates/amwapos-core/src/order_sync.rs`, `crates/amwapos-hub/src/order_sync.rs` |
| Settings screens | Admin → Automation → WhatsApp (connection, payment note) and Admin → Order sync |

## 1. WhatsApp connection: why WAHA

The official WhatsApp Business Platform (Meta Cloud API) is **not used**.
AMWAPOS offers two unofficial connections. Both speak the WhatsApp Web
protocol, so they act as a linked device of a normal WhatsApp or WhatsApp
Business phone number.

| | Built-in (default) | WAHA Core (recommended gateway) | Evolution API (not chosen) |
| --- | --- | --- | --- |
| What runs | Inside AMWAPOS (whatsapp-rust) | One Docker container next to AMWAPOS | API server plus PostgreSQL and Redis |
| Licence | Part of AMWAPOS | Apache-2.0 | Apache-2.0 with extra conditions |
| Moving parts | None | 1 container, 1 volume | 3 services |
| Engines | 1 | WEBJS (default), GOWS, NOWEB | Baileys, plus the official API |
| Can be shared with other tools | No | Yes (REST, dashboard) | Yes |

**Choice: WAHA Core**, when a gateway is wanted. It is a mature open-source
REST gateway (github.com/devlikeapro/waha). It runs as a single container
with nothing else to operate, it is free, and it needs no Meta verification.
Evolution API (github.com/EvolutionAPI/evolution-api) is also capable. It
needs a database and a cache to run, and those are more parts than a shop
needs for one number.

**When to keep the built-in connection:** a single shop computer, no Docker,
and nobody to look after a container. It has the same features: replies,
QR pairing, files and catalogue publishing.

**When to use WAHA:** the WhatsApp session should live on a server that stays
on (the hub machine or a small VPS), or other tools should share the same
number through WAHA's API.

### Practical implications

* **Session persistence.** WAHA keeps the paired session in its sessions
  volume (`/app/.sessions`). Keep that volume, back it up, and do not delete
  it, or the phone must scan the QR again. AMWAPOS keeps a small cursor file
  (`*.waha-cursor`) next to its WhatsApp data. After a restart it fetches the
  messages it missed, up to 6 hours back. Each message is stored once, keyed
  by its message id, so fetching again never makes a duplicate order.
* **Inbound is pulled, not pushed.** AMWAPOS polls WAHA, so the shop opens no
  inbound port and needs no public URL or webhook firewall rule. WAHA can stay
  bound to `127.0.0.1`.
* **Rate handling.**
  * Sends are paced at least 1.5 s apart, with jitter.
  * A 429 waits for WAHA's Retry-After.
  * A send whose answer was lost is checked against the chat before it is
    retried, so the customer never gets the same message twice.
  * The autopilot sends at most 6 replies per chat in 10 minutes. After that,
    a person takes the chat.
* **Ban risk.** Any unofficial client can be banned by WhatsApp. To keep the
  risk low:
  * use a dedicated shop number, not a personal one;
  * the autopilot only **replies** to customers who wrote first, and sends
    status updates only for orders they placed;
  * it never sends cold or bulk messages, and keeps no campaign features;
  * warm up a new number with normal use before it carries orders.
* **Self-host vs managed.** Self-host WAHA Core on the hub computer or a VPS
  you control. A managed WAHA host also works; give AMWAPOS its URL and key.
  The trade-offs:
  * a managed host means a third party holds the WhatsApp session and sees
    the messages;
  * self-hosting means you keep the container running and backed up.
* **Files.** Receipts are sent as files when WAHA accepts them. If WAHA refuses
  a file (for example, an engine or edition that does not support files),
  AMWAPOS sends the receipt as text instead. A refused file never stops a
  reply.
* **Catalogue.** Publishing products to the WhatsApp Business catalogue still
  uses the built-in connection
  ([WA_CATALOGUE_AUDIT.md](WA_CATALOGUE_AUDIT.md)). WAHA is used for messages
  only. Orders are always priced from the POS, not from the catalogue.

### Running WAHA (self-hosted)

```sh
docker run -d --name waha --restart unless-stopped \
  -p 127.0.0.1:3000:3000 \
  -e WAHA_API_KEY=<a long random key you generate> \
  -e WHATSAPP_DEFAULT_ENGINE=WEBJS \
  -v /srv/waha/sessions:/app/.sessions \
  devlikeapro/waha
```

Then, in AMWAPOS:

1. Admin → Automation → WhatsApp → Connection: choose **WAHA**, enter the URL
   (`http://127.0.0.1:3000`) and the session name (`default`), then save.
2. Paste the API key and choose **Save key**. The key is stored in the Windows
   credential store, shown only as "saved", and sent only to that WAHA
   address.
3. WhatsApp page: stop, then start. Scan the QR code with the shop phone
   (Linked devices).

If WAHA is on another machine, put it behind HTTPS (a reverse proxy) and
point AMWAPOS at the `https://` address. Never expose port 3000 to the
internet without TLS and the API key.

## 2. Turning the autopilot on

Feature flag **WhatsApp autopilot** (`orders.whatsapp_autopilot`). It works
only together with the flags it depends on: digital orders, WhatsApp, and AI
reading of WhatsApp orders. Recommended rollout:

1. Run with the autopilot **off** for a few days. The order inbox shows the
   drafts and the replies it would send, and staff send them. Check that
   products are matched and delivery zones and fees are filled in.
2. Fill in the delivery zones. An unknown fee hands the chat to a person
   instead of guessing.
3. Set the payment note (Automation → WhatsApp → templates →
   `autopilot_payment_note`).
4. Turn the autopilot on. When it is switched on, it does not send old status
   news to customers.

Staff can always step in. Taking over a chat (or replying by hand) silences
the autopilot in that chat until the takeover is released.

## 3. Customer flow

The AI only **reads** messages (items, quantities, address, intent). Every
reply is built by rules from POS data, so the autopilot never invents a
price, a product or a promise.

```
customer message
  └─ order pipeline: draft + questions + quote (POS prices, stock, zone fee)
       └─ autopilot plan
            ├─ question open   → ask it (item unclear, quantity, address, pickup or delivery)
            ├─ quote ready     → send the summary with lines, fee and total, ending "Reply YES to confirm"
            ├─ "yes"           → confirm, only if the summary sent is still exact (fingerprint)
            ├─ "cancel"        → cancel the draft, or a confirmed order not yet being prepared
            ├─ STATUS          → current state of the latest order
            ├─ HELP            → how to order (sent unasked at most every 12 h)
            └─ AGENT / problem → hand to a person (priority), autopilot goes quiet in that chat
```

* **Confirmation is exact.** The summary sent to the customer is
  fingerprinted: its lines, quantities, prices, address, fee and total. A
  "yes" confirms only that version. If anything changed since (a price, the
  stock, an edit by staff), the customer gets a new summary instead.
  Confirmation runs the same stock check, permissions and audit as a
  person's, under the system user `system:autopilot`.
* **Status updates after confirmation** come from the order event feed (§4).
  A change made anywhere (the till, the delivery board, another terminal)
  reaches the customer once:

| Order event | Customer message |
| --- | --- |
| `order.confirmed` | Confirmed, order number, total, payment note, delivery or pickup line |
| `order.preparing`, `order.pending` | (none, to keep the chat quiet) |
| `delivery.dispatched` | "On the way" (the `dispatch` template) |
| `delivery.delivered` | "Delivered" (the `delivered` template) |
| `delivery.not_delivered` | Could not deliver, the shop will contact you |
| `delivery.cancelled` | Delivery cancelled |
| `order.cancelled` | Cancelled (only if the customer had been sent a quote) |

When the autopilot is on, it owns these delivery messages. The older
per-delivery notices are skipped, so the customer never gets two.

### Commands

Commands are whole short messages, matched case-insensitively, ignoring
punctuation.

| Command | English | Arabic |
| --- | --- | --- |
| Status | `status`, `where is my order`, `track` | `الحالة`, `حالة الطلب`, `وين طلبي` |
| Person | `agent`, `human`, `staff`, `customer service`, `call me` | `موظف`, `ابي موظف`, `خدمة العملاء` |
| Help | `help`, `menu`, `how to order` | `مساعدة`, `القائمة`, `كيف اطلب` |

Replies use Arabic when the customer writes in Arabic.

### When a person takes over

The autopilot hands the chat to staff, marks it **priority**, and stays
silent in that chat in these cases:

| Reason code | When |
| --- | --- |
| `customer_asked_for_person` | AGENT command |
| `support_issue` | Complaint, refund, or problem with an order |
| `fee_unknown` | Address outside the configured delivery zones |
| `already_preparing` | Cancel or change asked after preparation started, or stock ran out at confirmation |
| `too_many_messages` | More than 6 automatic replies in 10 minutes (loop guard) |
| `unclear` | The message could not be understood after the usual questions |

It also leaves these to staff: messages more than 120 minutes old when
processed (AMWAPOS was off), and chats with no phone number (WhatsApp
`@lid` chats).

### States the customer sees

| Draft or order status | Meaning | Customer can |
| --- | --- | --- |
| draft (questions open) | Collecting items and address | answer, cancel |
| draft (quoted) | Summary sent | YES, change, cancel |
| `confirmed` | Accepted, stock reserved | STATUS, cancel |
| `preparing` / ticket rung up | Being prepared | STATUS, AGENT |
| delivery `dispatched` | With the rider | STATUS |
| delivery `delivered` / pickup completed | Done | — |
| `cancelled` / `not_delivered` | Closed | AGENT |

## 4. Order sync (event feed and adapters)

### Event feed

Database triggers on `digital_orders` and `delivery_orders` write one row to
`order_events` for each real state change. A save that leaves the state the
same writes nothing. Each row has:

* `seq`: a global order;
* `event_id`: unique, used for dedupe;
* `kind`: for example `order.confirmed`, `order.payment_recorded`,
  `delivery.dispatched`, `delivery.delivered`, `delivery.not_delivered`;
* `version`: increases by 1 per order, so a receiver can drop older states;
* `occurred_at`.

The triggers are in the database, so a change made by the till, the delivery
board, the autopilot, sync from another terminal, or a future screen is
always captured, without any code remembering to emit it.

Consumers keep their own cursor in the feed: the customer notifier and each
sync target. Events are pruned after 30 days, but only once every consumer
has passed them.

### Targets (adapters)

Admin → Order sync. Each target receives every event, in order, starting from
the moment it was added; history is not replayed.

**Webhook.** An HTTP `POST` of the event JSON. The headers are:

```
Content-Type: application/json
X-Amwapos-Event: order.confirmed
X-Amwapos-Event-Id: <event_id>
X-Amwapos-Signature: sha256=<hex HMAC-SHA256 of the raw body with the target's secret>
```

The secret has 32 random bytes. It is shown once, when the target is created,
and is kept in the credential store.

The receiver should do four things:

1. Verify the signature, using a constant-time comparison.
2. Ignore an `event_id` it has already stored. Delivery is at least once.
3. Ignore an event whose `version` is not newer than the one it holds for
   that `order.order_id`.
4. Answer 2xx.

To say "already have it", answer **409**. AMWAPOS counts 409 as delivered.

Body (abridged):

```json
{
  "event_id": "…", "seq": 1042, "kind": "order.confirmed", "source": "order",
  "version": 3, "occurred_at": "2026-10-12T09:14:03Z",
  "state": { "status": "confirmed", "payment_state": "unpaid" },
  "order": {
    "order_id": "…", "order_number": "WA-000123", "branch_id": "…", "channel": "whatsapp",
    "status": "confirmed", "payment_state": "unpaid", "customer_id": "…", "phone": "+973…",
    "delivery_wanted": true, "area": "…", "flat": "…", "building": "…", "road": "…", "block": "…",
    "delivery_fee_minor": 1000, "lines": [
      { "line_no": 1, "product_id": "…", "sku": "…", "name": "…", "qty_milli": 2000,
        "unit_price_minor": 450, "line_total_minor": 900 }
    ],
    "subtotal_minor": 900, "total_minor": 1900, "sale_id": null, "sale": null,
    "delivery_id": null, "external_ref": null, "created_at": "…", "updated_at": "…"
  },
  "delivery": null
}
```

Money is in minor units (fils for BHD, 1 BHD = 1000) and quantities are in
thousandths. `sale` (with `receipt_number`) appears once the order is rung up
at the till. `delivery` (with `delivery_number`, `status`, `outcome`,
`payment`, `amount_minor`) appears once a delivery exists.

**CSV file.** One line per event is appended to a file, for example on a
shared folder that a spreadsheet or an ERP import reads. The header is
written once:

```
event_id,seq,occurred_at,kind,version,order_number,order_status,payment_state,channel,phone,customer_id,delivery_number,delivery_status,delivery_outcome,receipt_number,total_minor,items
```

Cells that a spreadsheet would treat as formulas are neutralised, as in every
AMWAPOS export. After a crash, the file tail is checked, so an event is not
appended twice.

### Delivery semantics, failures and conflicts

* **In order, at least once.** A target never receives event N+1 before
  event N has been delivered or parked.
* **Responses to a webhook:**

  | Response | What AMWAPOS does |
  | --- | --- |
  | 2xx or 409 | Delivered |
  | 408, 425, 429, 5xx, timeout (15 s), network error | Retried, honouring `Retry-After` |
  | Any other 4xx | Refused at once |
  | Redirect | Not followed |

* **Backoff:** 10 s, 30 s, 2 min, 5 min, 15 min, 30 min, then 1 h.
* **Parking.** After 8 failed attempts, or a refusal, the event is
  **parked** and the target moves on, so one bad event cannot block the rest.
  Parked events are listed on the Order sync page with the error. Use **Send
  again** to queue them once more, or **Dismiss**.
* **AMWAPOS is the source of truth.** Sync is outbound only: nothing a
  receiving system answers changes an order. Conflicts are resolved on the
  receiving side by `version` (the newer state wins) and `event_id` (repeats
  are ignored). A receiver that falls behind catches up from the feed.
* **Test.** **Send a test** posts the latest event again (or a sample draft
  when there is none), with `"test": true` and an `event_id` starting with
  `test-`. The receiver can use it to check its signature code before real
  orders flow. A test does not move the target's place in the feed.
* The sync worker runs on the hub, or on a standalone computer. Terminals do
  not send, so there is one sender per shop.

### Adding a new kind of target

The core logic (feed, cursors, retries, parking) does not change. To add a
kind:

1. Hub: implement `OrderSink` in `crates/amwapos-hub/src/order_sync.rs`.
   `deliver(&SyncJob) -> SyncOutcome` returns `Delivered`,
   `Retry { error, after_s }`, or `Refused { error }`.
2. Add the kind in `build()`.
3. Core: accept the kind and validate its config in `order_sync_create`.
4. Widen the `kind` CHECK in a new migration.
5. UI: add the option in `src/screens/admin/orderSync.tsx`.

Most ERPs, accounting tools, Google Sheets (through Apps Script) and
automation tools (n8n, Make, Zapier) can receive the webhook as it is, with no
new adapter.

## 5. Assumptions

| Area | Assumption |
| --- | --- |
| Volume | One shop number, up to a few hundred orders and a few thousand messages per day. Pacing (1.5 s per send) limits a burst to about 40 replies per minute. |
| Region | Bahrain: BHD with 3 decimals, Bahraini addresses (Flat/Building/Road/Block), delivery zones by area. English and Arabic. |
| Store | One branch answers one WhatsApp number. More branches need one number (one WAHA session) each. |
| Payment | Paid on delivery or at pickup, as the payment note says. No online payment link is sent. |
| Existing systems | Reached by webhook or a CSV file. Inventory and POS are AMWAPOS itself. Stock is reserved at confirmation and sold at the till. |
| Network | The WAHA container is reachable from the hub. Sync targets are reachable over HTTPS, or the CSV folder is mounted. |
| Staff | Someone picks, packs and rides. The autopilot handles the conversation, not the physical fulfilment. |

## 6. Limitations

* Unofficial connection: WhatsApp can ban the number (see §1). There is no
  service-level agreement.
* Chats that WhatsApp shows only by a private id (`@lid`, with no phone
  number) are left to staff.
* Status messages older than 6 hours are not sent: AMWAPOS was off, and the
  news is stale.
* Sync is outbound only. Changes made in another system do not flow back
  into AMWAPOS.
* Not tested in this repository against a live WhatsApp number through WAHA.
  The tests use a fake WAHA server that follows WAHA's documented REST API.
  Pair a test number and run through §3 before going live.

## 7. Tests

* `crates/amwapos-core/tests/autopilot.rs` covers:
  * the full loop: message, quote, YES, confirmed, dispatched, delivered;
  * notices sent once each;
  * a stale YES after a price change;
  * cancel, then handoff;
  * AGENT;
  * unknown fee and stock changed;
  * the loop cap and the help limit;
  * Arabic and old messages;
  * switching on without sending old news.
* `crates/amwapos-core/tests/order_sync.rs` covers the feed, triggers,
  versions, cursors, parking, retry and dismiss, and pruning.
* `crates/amwapos-hub/tests/autopilot_sync.rs` covers the full WhatsApp to
  webhook path:
  * a 503, then a 200;
  * signature and order checks;
  * the CSV written once, with formulas neutralised;
  * how each HTTP status is classified.
* `crates/amwapos-hub/tests/waha.rs` runs against a fake WAHA server. It
  covers:
  * QR, connect and pull;
  * the automatic reply and confirmation;
  * no duplicate inbound messages;
  * a wrong API key;
  * a lost answer not re-sent;
  * the file fallback;
  * foreign media refused;
  * read receipts.
