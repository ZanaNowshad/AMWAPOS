# Supplier documents and WhatsApp orders

This file describes two features as they are built, including their limits:

- **Document Intelligence**: supplier invoices, credit notes and delivery notes.
- **WhatsApp AI orders**: customer chats become draft orders.

Both follow the same pattern:

> unstructured input → reading (rules first, AI only for what is left) → deterministic checks →
> structured draft → a person reviews → a person commits with the normal command.

Neither feature adds a new store of truth. Prices, stock, taxes, suppliers, customers, purchase orders, receiving and digital orders are the existing tables and commands. The AI can fill gaps in a draft; it cannot commit anything.

## 1. Supplier documents

### Flow

```
upload / WhatsApp attachment ──► invoice_scans (stage=uploaded, original kept in <data>/scans)
        │                          docs.import · docs.from_inbox
        ▼
OcrWorker (hub, own task) ── PDF text layer (lopdf) or rendered/embedded page image
        │   quality check → variants (contrast, small rotation) → 0/90/180/270 when the
        │   read looks sideways or weak → Tesseract TSV (eng+ara) → Layout (words, boxes)
        ▼
core docintel::service::analyze
        ├─ extract.rs   classify (invoice / credit_note / delivery_note / unknown),
        │               header fields, table lines (column-aware), packs, dates
        ├─ matching.rs  supplier (VAT > CR > learned alias > name > phone > fuzzy),
        │               lines (barcode > learned supplier code > SKU > exact name > fuzzy)
        ├─ checks.rs    line arithmetic, subtotal, VAT per line and document,
        │               duplicates, anomaly signals, PO candidates, reconciliation
        └─ summary     one plain sentence of what was read and what is wrong
        ▼
stage=ready_for_review ──► Admin → Supplier documents → review page
        │   corrections: docs.update / docs.update_line (revision-checked, audited)
        │   learning: supplier alias, supplier_product_map (only from a person's choice)
        ▼
explicit actions (purchasing.manage):
        docs.create_supplier_invoice → supplier_invoices (draft record, posting=not_supported)
        docs.create_receiving       → receiving_drafts (credit notes refused)
        ▼
receiving.draft_post (inventory.receive, operation id) → purchase_order_receive /
inventory_receive: the normal, idempotent, audited receiving that moves stock.
```

### Every value has provenance

- **Header fields.** Each field is a `Field<T>` with: value, raw printed text, evidence, source, band, status and note.
  - Evidence is the page, the line, a 0..1 box on the page, and the OCR confidence.
  - Source is `rules`, `ai`, `person` or `learned`.
  - Band is `high`, `medium`, `low` or `unresolved`.
  - Status is `ok`, `missing`, `conflict` or `corrected`.
- **Lines.** Each line keeps its printed text and evidence box, its match kind and score, its band, its reasons and its candidates.
- **Review screen.** It shows the page image. Clicking a field or a line draws its evidence box on the page.

### Checks (deterministic, integer fils)

- **Line arithmetic.** `qty × unit − discount = total`, with or without the line's VAT. When the columns are ambiguous, the printed arithmetic chooses the quantity: in "Tea 100 bags 5 0.850 4.250" the quantity is 5.
- **Totals.** Lines add up to the subtotal; subtotal + VAT = total. VAT is checked per line against the printed rate, else the product's configured rate, else the document rate. A rate that is not configured is flagged.
- **Messages are concrete.** For example: "Printed grand total: X BHD. Calculated from extracted lines: Y BHD. Difference: Z BHD."
- **"Use" versus "not an item".**
  - "Use" means *receive this line*. Turning it off leaves the line in the document checks, because it is still on the invoice.
  - "Not an item line" removes a misread row (a note, a header) from the checks as well.
- **Duplicates.**
  - The same file.
  - The same supplier and invoice number, including existing supplier invoice records.
  - The same invoice scanned differently (line fingerprint).
  - Possible duplicates (same supplier, total and date).
- **Anomaly signals.** These are reasons to look closer, never an accusation. Each one has a code:
  - `total_arithmetic`: totals that do not add up;
  - `vat_inconsistent`: VAT that does not match the applicable rates;
  - `duplicate_number`: an invoice number seen before;
  - `supplier_identifier_mismatch`: a VAT or CR number that differs from the supplier record;
  - `bank_details_changed`: an IBAN that differs from earlier documents of the supplier;
  - `unclear_key_value`: a key value read with low confidence;
  - `invoice_sequence`: a number lower than an earlier-dated invoice;
  - `price_deviation`: a price jump against the last purchase cost, raised only when the per-unit cost is known.
- **Reconciliation with a purchase order.**
  - PO candidates for the supplier are scored by PO number, reference and line overlap.
  - Each line gets a state: `fully_matched`, `quantity_variance`, `cost_variance`, `invoice_exceeds_received`, `missing_from_invoice` or `not_on_po`.
  - Variance is shown in BHD and %, with a threshold (`invoice_cost_variance_bp` in the inventory settings, default 5%).
  - The check becomes three-way once goods have been received on that PO.

### What is never automatic

- No stock movement.
- No cost change.
- No supplier liability. AMWAPOS has no payables ledger, so supplier invoice records say `posting: not_supported`.
- No approval of a discrepancy.
- No new catalogue product. `docs.new_product` returns a draft; with no pricing rule configured it says "No pricing rule is configured".
- No receiving from a credit note.

## 2. WhatsApp orders

### Flow

```
existing WhatsApp link (whatsapp-rust, same session) ──► wa_inbox (committed before ack)
        │  inbox_rev bump wakes the OrdersWorker (also every 3 s)
        ▼
OrdersWorker (hub, own task, never on the receive loop)
        └─ core wa_orders_process: claim once per message (wa_inbox_processing, INSERT OR IGNORE)
             ├─ interpret.rs  intent, items (qty, size, size word, variant), modifications,
             │                Bahrain address (flat/building/road/block/landmark), pickup/delivery,
             │                priority (complaint, urgent, waiting, large order);
             │                English, Arabic (incl. Arabic-Indic digits), Arabizi/Manglish lexicon
             ├─ resolve.rs    real products only, with current POS price and stock:
             │                alias (learned) > barcode > catalogue words; resolved only when one
             │                product clearly fits, else ambiguous with a question and real options,
             │                or unmatched; out of stock → unavailable + in-stock alternatives
             ├─ zones.rs      delivery fee from Settings → Delivery zones (block ranges, then area);
             │                no zone → fee unresolved (never invented)
             └─ one open session per chat (wa_order_sessions) whose draft is an ordinary
                digital_orders row (status draft, channel whatsapp)
        ▼
optional AI (orders.whatsapp_ai + AI provider): only for messages the rules left unresolved
        → schema-validated reply, product ids must be among the offered candidates,
          applied only if the conversation's revision has not changed, never to locked lines
        ▼
Admin → WhatsApp orders: conversation, draft with candidates, questions, delivery/zone,
customer link, payment evidence, suggested reply
        staff: waorders.line / delivery / customer / flags (take over, handled) / payment /
               send (queued on the normal outbox) / confirm / cancel
```

### Human boundary

- **Replies.** They are suggested, never sent by themselves. `waorders.send` needs `whatsapp.send` and goes through the normal idempotent outbox.
- **A customer's "yes".** It is recorded as an event (`customer_confirmed`), but only a person with `orders.manage` confirms. `waorders.confirm` goes through `orders.confirm`, and out-of-stock lines block it.
- **What a confirmed order is.** It is an ordinary confirmed digital order. A cashier loads it into a sale at the till (`orders.convert`) and takes payment as usual. Nothing is charged, no fiscal document is made, and stock does not move before that sale. Confirming holds the free stock for the order (`stock_reservations`, 48 hours). A hold is released on cancel, converted on sale, and expired by the hub maintenance loop.
- **Confirm re-checks.** At confirmation, stock is checked again. The total is also compared with the last total sent to the customer. Either difference is a `conflict` with `details.kind` = `stock_shortage` or `price_changed_since_quote`. The UI explains it in plain words and offers "Confirm anyway", which sends the matching `acknowledge_*` flag and records it in the audit log.
- **Payment screenshots.** They are linked to the draft as evidence (`payment_state = screenshot_pending`). Only a person with `payments.review` marks them verified.
- **Staff take over.** New messages are then shown, but they no longer change the draft.
- **Staff corrections are locked.** A later reading, from the rules or the AI, never overwrites them. When "Remember my choices" is ticked, the correction teaches a product alias.

### Idempotency and concurrency

- One processing row per inbound message. A redelivered message or a restart does nothing twice.
- There is one open session per chat, enforced by a unique index.
- Staff edits carry the session revision, and a stale revision is refused. The AI apply checks the revision too.

### The order journey in the UI (first-time users)

The admin menu has one group, **Orders & delivery**, ordered the way an order moves:

1. WhatsApp orders
2. Orders
3. Deliveries
4. Payment checks
5. Customers

Every one of these pages starts with the same journey bar, built from `orders.flow`:

> Messages → To confirm → To pack & send → On the way → Payments to check

Each step shows how many items are waiting and links to its page. Steps a person cannot act on (no permission, or the module is off) are left out.

Each WhatsApp order opens with one **next step**: the single thing to do now (`waNextStep` in `waOrders.tsx`), in this order:

1. choose the right product
2. find unmatched products
3. replace out-of-stock items
4. delivery or pickup
5. complete the address
6. pick the customer
7. confirm

After confirming, the card says what happens next.

Other screens:

- **Deliveries.** Board and list open the same ticket sheet.
- **Rider desk.** Each drop has one large next-step button (Start packing → Picked up, on the way → Delivered), plus Call and WhatsApp buttons. Marking an unpaid drop delivered opens the ticket sheet, which asks about the money.
- **Status words.** They are plain: New, Packing, On the way, Delivered.
- **Cancelling an order.** It always asks first.

## 3. AI use (both features)

- **One AI layer.** Both features reuse the existing one: provider settings and keys in the credential store, `ai_client::complete_once`, and the fallback model. There is no second provider.
- **Narrow prompts.**
  - Documents send the numbered OCR lines (at most 20,000 characters) and the catalogue candidates of each line.
  - WhatsApp sends the message (at most 600 characters, with phone numbers redacted), the current draft lines and the candidates.
  - Neither sends customer names, phone numbers, balances, history or the database.
- **Validated output.** `docintel/aischema.rs` and `waorders/aischema.rs` reject:
  - non-JSON replies and unknown fields;
  - invalid money or quantities, and dates that are not real;
  - product ids outside the offered candidates;
  - evidence line numbers that are not on the page.

  A rejected reply is recorded as `ai_rejected`, and the rules result stays.
- **No chain-of-thought is stored.** `ai_decisions` keeps structured evidence only: what changed, the source and the model name.
- **Assistant tools.** The assistant's tool map (`ai_tools.rs`) gained read tools and two low-risk line-correction proposals:
  - reads: `document_get`, `document_metrics`, `list_receiving_drafts`, `receiving_draft_get`, `list_supplier_invoices`, `supplier_invoice_get`, `list_whatsapp_orders`, `whatsapp_order_get` and `whatsapp_order_metrics`;
  - proposals: `propose_document_line` and `propose_whatsapp_order_line`.

  Posting, approving, confirming, paying and sending are listed as forbidden, each with its reason.

## 4. Permissions

| Action | Permission |
|---|---|
| Upload / read / correct documents | `ocr.scan` |
| Supplier invoice record, receiving draft, draft edits, invoice approve/void | `purchasing.manage` |
| Post a receiving draft (moves stock) | `inventory.receive` |
| Read WhatsApp orders | `orders.manage`, `whatsapp.manage` or `whatsapp.send` |
| Edit / confirm / cancel WhatsApp orders | `orders.manage` |
| Send a reply | `whatsapp.send` |
| Verify a payment screenshot | `payments.review` |

## 5. Settings and flags

- **Document flags.** `ocr.enabled` → `ocr.supplier_invoices` → `ocr.ai_parse` (AI help for unresolved values).
- **Order flags.** `whatsapp.enabled` + `orders.digital` → `orders.whatsapp_ai` → `orders.whatsapp_upsell`. Upsell suggestions are shown to staff only.
- **Delivery zones.** Settings → Delivery → Delivery zones: name, fee, "free over", blocks (`200-260, 301`), areas, active.
- **Cost variance threshold.** `invoice_cost_variance_bp` in the `inventory` settings (default 500 = 5%). It is validated and saved through `settings.save`, but there is no screen field for it yet.

## 6. Observability

- `docs.metrics`: documents, read/failed, extraction success, OCR failure, supplier and product auto-match, correction rate, duplicates, average processing time.
- `waorders.metrics`: messages processed, order intents, drafts, confirmed, conversion, clarification rate, product resolution, staff overrides, failed jobs.
- Both are shown above their lists. Workers log stage changes and failures without document contents or session material.

## 7. Tests and evaluation

- **Core document tests** (`crates/amwapos-core/tests/docintel.rs`): drafts without posting, permissions, learning, duplicates and anomalies, PO three-way, credit notes, AI validation, "Use" versus "not an item", and unclear packs.
- **Real OCR tests** (`crates/amwapos-hub/tests/document_intelligence.rs`): rendered English, rotated, blurred and Arabic pages, a two-page PDF, and damaged or protected PDFs.
- **Core order tests** (`crates/amwapos-core/tests/waorders.rs`): the example conversation, questions and spam, modifications, Arabic and Manglish, staff overrides and takeover, idempotency, customers, payment screenshots, priority, and AI validation.
- **WhatsApp orders through the runtime** (`crates/amwapos-hub/tests/whatsapp_orders.rs`): uses the fake WhatsApp adapter, from a delivered message to a draft, a staff reply on the outbox, a customer "yes" that confirms nothing, and a person's confirmation with no sale, payment or stock movement.
- **Browser end to end** (`e2e/whatsapp_documents.spec.ts`):
  - real Tesseract on a synthetic invoice image, then review with the evidence box, a receiving draft, and a person receiving the stock;
  - a WhatsApp chat with a size question answered by staff, a reply sent, and the order confirmed.

  The dev bridge runs with `--fake-whatsapp`.
- **Evaluation** (`crates/amwapos-core/tests/eval.rs`, data in `tests/fixtures/eval/`):
  - The data is 9 synthetic documents and 16 synthetic chats in English, Arabic, Manglish and mixed.
  - Each case runs the real pipeline and is compared per field.
  - The test fails on any wrong product or supplier (unsafe), or on anything committed automatically.
  - Floors of 85% per field guard against regressions.
  - Current result: 100% on every field. `AMWAPOS_EVAL_REPORT=file.json` writes the report.
  - The set is small and synthetic, so it shows that the pipeline behaves as designed, not how accurate it is on real-world documents.

## 8. Known limitations

- **PDFs.**
  - PDF text is read without positions, so text PDFs have no evidence boxes; scanned PDFs do.
  - JBIG2 and CCITT images inside PDFs are reported, not decoded.
  - Protected PDFs are refused with a message.
- **Handwriting** is best-effort; Tesseract is not a handwriting reader.
- **Image barcodes are not decoded.** Barcodes come from the printed digits.
- **Live AI not verified.** The AI provider path is covered with validation tests and the offline provider, not against a live model in this environment.
- **Live WhatsApp not verified.** WhatsApp is exercised with the fake adapter; the real link must be checked once on a real number.
- **No payables ledger.** Supplier invoice records are review records only.
- **Rule vocabulary is finite.** Interpretation is rule-based with a lexicon. Unusual phrasings become `unknown` or unresolved and are left to a person (or the AI, when configured). They are never guessed.
