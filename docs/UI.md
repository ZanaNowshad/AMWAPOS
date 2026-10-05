# UI contract (1024×768, touch-first)

The primary cashier display is **1024×768 CSS pixels at 100% scaling**, driven by
touch; mouse, keyboard and a barcode scanner (keyboard wedge) still work. Admin
runs on the same panel.

Tokens live in `src/styles/tokens.css` (light and dark). Components use them;
no new colours in components.

## Tokens

| Token | Value / use |
| --- | --- |
| Look ("Amwaj" v2) | Calm paper surfaces (`--bg` with a soft teal glow), a night-ink chrome (`--brand-dark`), soft layered shadows (`--shadow-sm/md/lg`). |
| `--brand` (`--pos-accent`) | Deep sea teal `#08747e` (dark: `#3cc4cb`). The one accent. |
| `--pos-pay` / `--pos-pay-grad` | PAY only: emerald `#0c8448` (dark: `#2fbf68`). |
| `--sand` | Badges and counts on the dark chrome only. |
| `--danger`, `--warning`, `--success` | One each. |
| `--radius` / `--radius-card` / `--radius-modal` | 12 / 16 / 20 px. |
| `--pos-touch-min` / `--pos-touch-lg` | 48 / 60 px. |
| `--pos-top-h` / `--pos-dock-h` | 56 / 88 px. |
| `--money-figures` | `tabular-nums lining-nums` for every amount (`.money`, `.num`). |
| `--dur` / `--dur-press` | 170 ms / 60 ms; `prefers-reduced-motion` makes both 0. |

Compact density is the default (`data-density="compact"`): tighter rows, same
48 px hit targets.

## Rules

- Hit targets ≥ 48×48 px (small buttons extend their hit area with `::after`),
  8 px between neighbours; POS primary actions 56–64 px.
- Every control has default, pressed (≤ 80 ms), disabled and focus-visible
  states; hover is pointer-only (`@media (hover: hover)`), never the only way.
- Body 16 px, secondary 14 px, line height ≥ 1.25; POS totals 36 px; prices
  never wrap (`.money { white-space: nowrap }`).
- Logical properties only; `dir="rtl"` mirrors nav and the cart/search columns.
- One sheet at a time (payment, refund, shift close, More, AI sheets). Esc and a
  48 px ✕ close them.
- The scan field keeps the keyboard except while a text field or the AI
  composer has focus.

## Cashier layout

The till lists no catalogue: products are added by scanning or by typing in the
scan field (results drop down under it).

```
| 56  brand · shift · backup/sync/printer pills · Held · clock · Send · AI · More |
| scan / search field (56) ...................... | checkout column (340):      |
| cart lines (64 min): product tile · name ·      |  customer card (F3)         |
|   qty stepper · line total · remove             |  Hold · Sale discount · Refund |
|                                                 |  last sale (when empty)     |
|                                                 |  subtotal · discount · VAT · items |
|                                                 |  TOTAL (34)                 |
|                                                 |  PAY full width × 72        |
```

At 125 % / 150 % display size (or a window under 900 px) the checkout column
folds into the 88 px dock (sums · TOTAL · PAY 224×64) and Customer, Hold,
Sale discount and Refund move back beside the scan field. The Send rail and
the till assistant open over the cart, never over the checkout column.

## Words and names

- Status words are plain: New, Packing, On the way, Delivered; To review, Ready to post, Posted.
- Product and customer names carry `dir="auto"`: a Latin name in the Arabic till keeps its own
  direction, so it is cut at its end. In the cart line the barcode gives way first; the price and
  the stock/discount chips are never clipped.
- Amounts typed back into a field use the plain amount (`formatAmount`), never a piece of a
  formatted money string (in Arabic that carries invisible direction marks; a test forbids it).
- Settings fields have translated labels; rates are typed as %, money as amounts.
- Labels are sentence case ("Add product", "Back up now", "Log out"); a primary button is a verb
  ("Add customer", not "+ Customer"). Subtitles say what the page is for in shop words, never
  "immutable", "ledger" or "SKU".
- Table dates use no-break spaces and codes (`.mono`) never wrap, so a row stays on one line.
- Date ranges show presets first; exact dates appear behind "Custom dates".
- Every screen has exactly one `h1` (screen-reader only on the till, setup steps and AI).
- Settings sections live in the address (`?section=`): every "Open Settings → …" link lands on
  its section. Admin mode is kept across a reload for the session.
- The full per-screen inventory is [PRODUCT_UX_FORENSIC_AUDIT.md](PRODUCT_UX_FORENSIC_AUDIT.md).

## Payment sheet

Quick-cash buttons offer the bill rounded up to the next 1, 5, 10, 20, 50 and 100 (at most four,
all covering the bill). The change is shown large above the keypad and again next to the Complete
button, because at 1024×700 the change panel can be below the fold.

## Admin navigation

- Groups follow the work: Overview, Orders & delivery (WhatsApp orders → Orders → Deliveries →
  Payment checks → Customers), Sales, Catalog, Inventory, Purchasing (incl. Payables), Business,
  Automation, System.
- A module that is off is not listed; its page still says "Not enabled" for direct links, and it is
  turned on in Settings → Features.
- System shows Users & Roles, Backups and Settings; the rest is under "More tools", which opens by
  itself on those pages and remembers its state on this computer.
- Every order page starts with the journey bar (Messages → To confirm → To pack & send → On the way →
  Payments to check), showing only the steps this person can act on.

## Wave 1 surfaces

- The till's Recent sales has **Void sale** (`void-sale`). The void dialog (`void-dialog`)
  offers reason chips, shows the amount, and asks for a manager when needed. A voided sale
  stays in the list with a **Voided** chip.
- Admin → Business → **Expenses** has KPIs (spent this month, waiting for approval, approved
  but not paid) and three tabs:
  Expenses, Petty cash and Repeats.
  - Expenses: the editor (VAT only behind "The bill shows VAT") and a drawer with
    Approve / Reject / Pay / Void.
  - Pay dialog: the method as chips.
- Customer → **Account**: the statement card (`statement`) shows ageing buckets (late ones in
  red), the ledger table, PDF and WhatsApp, and **Days to pay**.
- Admin → Sales → receipt preview shows the receipt fingerprint, or "reconstructed" for old
  records.

## Display size

Settings → Appearance → Display size (90, 100, 110, 125, 150 %) zooms the whole
app on that computer (the WebView's own zoom; CSS zoom in a browser). At 125 %
a 1024×768 panel behaves as 819×614 CSS px and at 150 % as 683×512: the header
drops the business name and clock, the dock keeps TOTAL and PAY only below
760 px, and the AI page folds its side columns into sheets. The layout spec
checks the till and the payment sheet at both sizes.

PAY ends at least 16 px above the bottom edge (column and dock), so a 16 px
taskbar overlap never hides it. The till assistant opens over the search column, between the top bar and
the dock, and never covers PAY.

## Proof

`e2e/layout1024.spec.ts` runs these states at 1024×768 and saves screenshots
when `E2E_SHOTS=<dir>` is set: empty cart; 12 lines + low stock + held ticket;
payment (cash + change, and PAY/Confirm again at 1024×700); shift close; refund
step 2; AI empty / tools + thinking / high-risk proposal with diff; till
assistant over a 6-line cart; Arabic POS + assistant; dark compact; backup
overdue + hub "Update needed" with the admin banner and nav flyout.
