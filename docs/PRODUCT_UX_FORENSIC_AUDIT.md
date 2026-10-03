# Product / UX forensic audit

This is the inventory of every user-facing surface in AMWAPOS. Each one was
looked at as a first-time user and as an expert, in English and Arabic, at
the 1024×768 till panel. For each surface it records 15 fields and one
classification. Where a problem could be fixed in this repository it was
fixed, and a regression check was added. Changing the product means changing
this file.

Keep it current by hand: when a surface changes, update its entry.

## How it was audited

1. **Automated sweep — every Admin destination** (`e2e/surfaces.spec.ts`).
   The owner, with every optional module on, opens all 43 Admin routes, all
   18 Settings sections and the record pages (product, customer, supplier,
   report) in English and then in Arabic at 1024×768. Each page must:
   - have a page heading (`main h1`);
   - finish loading (no `aria-busy` after 15 s);
   - show no error banner and throw no script error;
   - never scroll sideways;
   - in Arabic, show no English interface text (headings, labels, buttons,
     tabs, table headers, menu). User data marked `dir=auto` and money,
     numbers and codes are excluded; brand and protocol words are allowed;
   - for Settings, open the section the link names.
2. **Role sweep** (same file). Manager, accountant and inventory staff sign
   in through the UI; every Admin link they are shown must open cleanly. A
   link the backend would refuse is a defect.
3. **Cashier and layout** (`e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts`):
   setup → first sale → refund → shift close → reports → admin, plus the till
   at 1024×768, 1024×700 and 125 % / 150 % scaling, in English, Arabic and
   dark mode.
4. **Manual review** of every screenshot from those runs (`E2E_SHOTS=<dir>`
   saves them), answering for each screen: where am I, what is it for, what
   should I do, what can I ignore, what happens next.
5. **Code review** of every Modal, Drawer and Confirm (counted from the
   source: 151 titled surfaces in 25 files) and of the backend command
   behind each primary action.

## Coverage

| Surface group | Count |
|---|---|
| Setup and sign-in | 8 |
| Cashier mode | 18 |
| Admin destinations (incl. record pages) | 48 |
| Settings sections | 18 |
| **Screens with the full 15-field record** | **92** |
| Dialogs, drawers and confirmations (grouped below) | 73 rows covering 151 titled surfaces |

| Classification | Screens |
|---|---|
| DEFECT FIXED | 27 |
| EXTERNAL/HARDWARE VERIFICATION REQUIRED | 9 |
| IMPROVEMENT IMPLEMENTED | 32 |
| REVIEWED — NO CHANGE NEEDED | 24 |

Classifications: REVIEWED — NO CHANGE NEEDED · IMPROVEMENT IMPLEMENTED ·
DEFECT FIXED · EXTERNAL/HARDWARE VERIFICATION REQUIRED · INTENTIONALLY
DEFERRED (with reason).

## First-time journey

| Step | What a new owner sees | Finding | Outcome |
|---|---|---|---|
| Setup | Welcome, then one question per step with a rail of steps | Steps after Welcome had no page heading | Fixed (screen-reader `h1`) |
| Sign in | Name tiles, then PIN | Two page headings | Fixed |
| Start shift | Opening float | “Start Shift” casing | Fixed |
| First sale | Scan box, cart, Pay | Till had no page heading | Fixed |
| Refund | More → Refund → find → choose → review → done | — | Reviewed |
| Close shift | Count, variance, done | — | Reviewed |
| Reports | Catalogue → report | Failed report showed a skeleton forever | Fixed |
| Admin | Dashboard with attention lines | Repeated “No data for the same day last week” | Fixed |
| System health | Diagnostics, Backups | Backup restore placeholder showed `\\\\`; raw type codes | Fixed |
| Reload while in Admin | Admin | Reload dropped the owner back to the till | Fixed (mode kept for the session) |

## Severity summary

| Severity | Found | Fixed | Remaining |
|---|---|---|---|
| Critical | 0 | 0 | 0 |
| High | 3 | 3 | 0 |
| Medium | 12 | 12 | 0 |
| Low | 21 | 21 | 0 |

High: Settings deep links ignored when Settings was open; report page stuck
loading beside its error; reload in Admin lost the mode. Medium: Arabic
interface falling back to English on Roles, Reports, purchase-order statuses
and date presets; duplicate supplier-invoice list; missing page headings
(AI, Profile, till, setup, login double heading); order-journey steps with no
accessible name; low-contrast connection pill on the Admin bar; table dates
wrapping and hiding the Shifts status. Low: Title Case and jargon labels,
clipped selects, single-tab product editor, empty transfers guidance, raw
codes on Devices/Backups, doubled backslashes, AI language chip “UI”,
user-data chips not `dir=auto`.


## Setup and sign-in

### Setup — Welcome / path choice

Route: `first run` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Choose how this computer is used: single store, store hub, or till joining a hub.
- **Primary user:** Owner on day one
- **Primary task:** Pick the right path once
- **Primary action:** Choose a path card, Continue
- **Current friction:** None found.
- **First-time ambiguity:** “Hub” is explained on the card (“other tills pair with this computer”).
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Left rail lists every step with the current one marked (`aria-current=step`); Back is disabled on the first step.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Welcome step has the page `h1`; later steps now also have one (screen-reader-only “AMWAPOS setup”) — previously they had only `h2`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** Added a screen-reader `h1` on steps 2+; “Finish Setup” → “Finish setup”.
- **Verification evidence:** `e2e/checkout.spec.ts` (setup through first sale)

### Setup — Business, Branch, Tax, Owner, Terminal, Receipt, Printer, Backups, Review

Route: `first run` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Collect only what the first sale needs; everything is editable later in Settings.
- **Primary user:** Owner
- **Primary task:** Fill each step
- **Primary action:** Continue
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Back/Continue footer; the rail shows done steps with a tick.
- **State / feedback:** Inline validation per field; the backend validates again on Finish and a failure returns to the step with the message.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`

### Setup — Connect to the hub (join path)

Route: `first run` — **EXTERNAL/HARDWARE VERIFICATION REQUIRED**

- **Purpose:** Pair this till with the store hub.
- **Primary user:** Owner / installer
- **Primary task:** Enter hub address and pairing code
- **Primary action:** Pair terminal
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Pairing errors (unreachable, wrong code, version mismatch) are named separately.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** Hub pairing covered by Rust tests (`crates/amwapos-hub/tests`); a second physical computer on the store LAN is required for acceptance

### Login — who is signing in

Route: `/ (signed out)` — **DEFECT FIXED**

- **Purpose:** Pick your name, then enter your PIN.
- **Primary user:** Every staff member
- **Primary task:** Sign in
- **Primary action:** Name tile → PIN → Log in
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Locked accounts show “Locked” on the tile and an explanation on the PIN step; wrong PIN shows the remaining attempts from the backend.
- **Accessibility:** Had two `h1`s (brand tagline and the question). The tagline is now a styled paragraph; “Who is signing in?” is the only heading.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** Tagline demoted from `h1` to paragraph (styling kept).
- **Verification evidence:** `e2e/checkout.spec.ts` (EN and AR sign-in by heading)

### PIN entry / keypad

Route: `/ (signed out)` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Enter a 4–8 digit PIN.
- **Primary user:** Staff
- **Primary task:** Type PIN
- **Primary action:** Log in (or Enter)
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`

### Lock screen

Route: `idle lock` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Hide the till after inactivity; unlock with the same person's PIN.
- **Primary user:** Cashier
- **Primary task:** Unlock
- **Primary action:** PIN → Unlock
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** Code review (`LockScreen.tsx`); timeout from Settings → Security

### Windows Hello unlock

Route: `lock / login` — **EXTERNAL/HARDWARE VERIFICATION REQUIRED**

- **Purpose:** Unlock with the computer's biometrics instead of a PIN.
- **Primary user:** Staff on a Windows Hello device
- **Primary task:** Unlock
- **Primary action:** Use Windows Hello
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** Needs a Windows device with Hello hardware

### Start shift

Route: `/ (no open shift)` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Count the opening float before selling.
- **Primary user:** Cashier
- **Primary task:** Enter opening cash
- **Primary action:** Start shift
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “Start Shift” → “Start shift” (sentence case like every other button).
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** Label sentence-cased; e2e selectors updated.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts`

## Cashier mode

### Till (sale screen)

Route: `/` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Scan or tap products, see the total, take payment.
- **Primary user:** Cashier
- **Primary task:** Ring up a sale
- **Primary action:** Pay (F12)
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Header: shift chip, connection pill, language, More (all secondary tools), Admin for permitted users.
- **State / feedback:** Scan box keeps focus; unknown barcode opens its dialog; offline never blocks a sale.
- **Accessibility:** The till had no page heading; a screen-reader `h1` “Checkout” now names it.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** Added screen-reader `h1`. Connection pill text “Local” → “This till” / “Store hub” with an explaining tooltip.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### More menu

Route: `/ → More` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Every secondary till tool in one sheet (hold, held, refund, cash in/out, recent sales, print queue, close shift, Admin).
- **Primary user:** Cashier
- **Primary task:** Find a tool
- **Primary action:** Tap the tool
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “Logout” (one word, noun) → “Log out” here, in Admin and in the delivery desk; cart title “Current Sale” → “Current sale”.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** Labels changed; e2e selectors updated.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Payment sheet

Route: `/ → Pay` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Choose tender, enter amount, finish.
- **Primary user:** Cashier
- **Primary task:** Take payment
- **Primary action:** Complete sale
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “Amount Due”, “Complete Sale” in Title Case unlike the rest of the till.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Quick-cash buttons, exact amount, change due shown large; double tap is idempotent (operation id).
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** “Amount due”, “Complete sale”.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Sale completed

Route: `/ → after payment` — **EXTERNAL/HARDWARE VERIFICATION REQUIRED**

- **Purpose:** Confirm the sale and offer print / WhatsApp receipt.
- **Primary user:** Cashier
- **Primary task:** Hand over change and receipt
- **Primary action:** New sale
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`; physical receipt printer output needs hardware

### Refund — find receipt

Route: `/ → More → Refund` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Find the original sale.
- **Primary user:** Cashier / manager
- **Primary task:** Look up receipt
- **Primary action:** Find
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Refund — choose lines, Review refund, Refund completed

Route: `/ → More → Refund` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Pick what comes back, check totals, confirm.
- **Primary user:** Cashier with approval
- **Primary task:** Refund
- **Primary action:** Refund
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Approval prompt when the role lacks the permission; review step before money moves.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Close shift / Shift closed

Route: `/ → More → Close shift` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Count the drawer and close the shift with a variance.
- **Primary user:** Cashier
- **Primary task:** Count cash
- **Primary action:** Close shift
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/layout1024.spec.ts`, `e2e/checkout.spec.ts`

### Barcode not found

Route: `/ (scan)` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Explain the unknown barcode and offer custom item or later resolution.
- **Primary user:** Cashier
- **Primary task:** Keep selling
- **Primary action:** Sell as custom item / dismiss
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Hold sale / Held sales / Prices changed since held

Route: `/ → More` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Park and resume sales; confirm price changes on resume.
- **Primary user:** Cashier
- **Primary task:** Resume a sale
- **Primary action:** Resume
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Customer picker / New customer

Route: `/ → Customer` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Attach a customer to the sale.
- **Primary user:** Cashier
- **Primary task:** Find or add customer
- **Primary action:** Select
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Quantity / Discount / Change price / Custom item

Route: `/ (line)` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Edit a line.
- **Primary user:** Cashier
- **Primary task:** Correct a line
- **Primary action:** Apply
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Discounts above the limit ask for approval (`approval.tsx`).
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Cash in / Cash out / Safe drop / No sale

Route: `/ → More` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Record cash movements with a reason.
- **Primary user:** Cashier
- **Primary task:** Record cash
- **Primary action:** Record
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Recent sales / Print queue

Route: `/ → More` — **EXTERNAL/HARDWARE VERIFICATION REQUIRED**

- **Purpose:** Reprint and see what is waiting to print.
- **Primary user:** Cashier
- **Primary task:** Reprint
- **Primary action:** Reprint
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** Printing output needs a receipt printer

### Delivery / Redeem points

Route: `/ → Customer` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Send the sale out for delivery; spend loyalty points.
- **Primary user:** Cashier
- **Primary task:** Set delivery or redeem
- **Primary action:** Apply
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Send loop (ticket, paid?, record payment, rider hand-over, unable to deliver, close as not delivered)

Route: `/ → Send` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Track a delivery ticket from packing to the door.
- **Primary user:** Cashier / rider
- **Primary task:** Move the ticket on
- **Primary action:** Next step button
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/whatsapp_documents.spec.ts`, Rust send-loop tests

### Till orders / Order confirm / Approval prompt

Route: `/ → Orders` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Ring up confirmed orders; manager approval for restricted actions.
- **Primary user:** Cashier / manager
- **Primary task:** Sell an order
- **Primary action:** Ring up
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### AI helper drawer (till)

Route: `/ → AI` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Ask about the current cart or stock.
- **Primary user:** Cashier
- **Primary task:** Ask
- **Primary action:** Send
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/checkout.spec.ts`, `e2e/layout1024.spec.ts` (EN and AR screenshots at 1024×768 and 1024×700)

### Offline / hub unreachable

Route: `header pill` — **EXTERNAL/HARDWARE VERIFICATION REQUIRED**

- **Purpose:** Say plainly that selling continues.
- **Primary user:** Cashier
- **Primary task:** Keep selling
- **Primary action:** None needed
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Opened from the till; closes back to the sale with Esc or the close button.
- **State / feedback:** Primary button spins and is disabled while the backend works; errors appear inside the sheet in plain words; nothing is recorded until the backend confirms.
- **Accessibility:** Modal with title, focus trap, Esc to close, labelled inputs; amounts use `.money` with `dir=ltr`.
- **Responsive / touch:** Keypad keys and primary buttons 56–64 px; fits 1024×768 and 1024×700 (`e2e/layout1024.spec.ts`).
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** Rust sync tests; a real network outage on store hardware is part of acceptance

## Admin destinations

### Dashboard

Route: `/admin/dashboard` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Today at a glance and what needs attention.
- **Primary user:** Owner / manager
- **Primary task:** See whether anything needs doing
- **Primary action:** Open an attention line
- **Current friction:** Every KPI on a new store repeated “No data for the same day last week”.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** The repeated line said nothing useful.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** The comparison line is hidden when there is no last week to compare with.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### WhatsApp orders

Route: `/admin/whatsapp-orders` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Chats read into draft orders.
- **Primary user:** Order taker
- **Primary task:** Confirm a draft order
- **Primary action:** Confirm order
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Orders

Route: `/admin/orders` — **DEFECT FIXED**

- **Purpose:** Phone/WhatsApp/web orders until sold.
- **Primary user:** Order taker
- **Primary task:** Confirm and hand to the till
- **Primary action:** New order / Confirm
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** Order-journey steps hide their words on narrow screens, so a screen reader heard only “0 waiting”. Each step now has a full name (“To confirm: 0 waiting”) and a tooltip.
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Accessible names and tooltips on every journey step.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Deliveries

Route: `/admin/deliveries` — **DEFECT FIXED**

- **Purpose:** Every order going out, board or list.
- **Primary user:** Dispatcher
- **Primary task:** Move deliveries on
- **Primary action:** Open a card
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** Area and rider filter chips show names people typed; in Arabic they were laid out as interface text (left-to-right Latin names inside RTL chips).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Those chips are `dir=auto`; found by the Arabic sweep.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Payment checks

Route: `/admin/payment-reviews` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Compare transfer screenshots with the bank.
- **Primary user:** Owner
- **Primary task:** Accept or reject
- **Primary action:** Open a review
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Customers

Route: `/admin/customers` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Customer list and accounts.
- **Primary user:** Manager
- **Primary task:** Find or add a customer
- **Primary action:** Add customer
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** Primary button read “+ Customer” (a noun).
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Now “Add customer”.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Customer record

Route: `/admin/customers/:id` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Profile, addresses, balance, history.
- **Primary user:** Manager
- **Primary task:** Take payment / adjust
- **Primary action:** Take payment
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** Customer name heading is `dir=auto` so Arabic and English names read correctly.
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Sales

Route: `/admin/sales` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Completed sales.
- **Primary user:** Manager
- **Primary task:** Find a receipt
- **Primary action:** Open a sale
- **Current friction:** Dates and receipt numbers wrapped onto two lines.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** Subtitle said “Records are immutable”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Subtitle: “A sale is never edited; to correct one, make a refund.” Dates use no-break spaces; codes never wrap.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Refunds

Route: `/admin/refunds` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Refund history.
- **Primary user:** Manager
- **Primary task:** Check refunds
- **Primary action:** Open a refund
- **Current friction:** Dates wrapped.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No-break dates.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Shifts

Route: `/admin/shifts` — **DEFECT FIXED**

- **Purpose:** Shift history and variances.
- **Primary user:** Manager
- **Primary task:** Check a shift
- **Primary action:** Open a shift
- **Current friction:** Dates wrapped and pushed the Status column off the card.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No-break dates; Status visible.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Cash

Route: `/admin/cash` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Cash in/out/drops.
- **Primary user:** Manager
- **Primary task:** Check cash events
- **Primary action:** Filter
- **Current friction:** Custom date inputs always visible next to the presets.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Date range shows presets plus “Custom dates”; inputs appear only when needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Products

Route: `/admin/products` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Catalogue.
- **Primary user:** Manager
- **Primary task:** Find / add / archive
- **Primary action:** Add product
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “Add Product”, stock chips “In Stock / Low Stock / Out of Stock”, pager “50 / p…” clipped.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Sentence case everywhere; page-size select sized to its words.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Product editor (new)

Route: `/admin/products/new` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Create a product.
- **Primary user:** Manager
- **Primary task:** Name, barcode, price
- **Primary action:** Save
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** A tab bar with a single “General” tab.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Tab bar hidden until the product exists and has more than one tab.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Product editor (existing)

Route: `/admin/products/:id` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Edit details, barcodes, prices, stock, history.
- **Primary user:** Manager
- **Primary task:** Edit
- **Primary action:** Save
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “Add Barcode” → “Add barcode”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Leaving with unsaved changes asks first.
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Label sentence-cased.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Categories

Route: `/admin/categories` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Groups for the till and reports.
- **Primary user:** Manager
- **Primary task:** Add / archive
- **Primary action:** Add category
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “Add Category” → “Add category”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Sentence case.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Pricing

Route: `/admin/pricing` — **DEFECT FIXED**

- **Purpose:** Bulk price changes with preview.
- **Primary user:** Manager
- **Primary task:** Change prices
- **Primary action:** Apply price changes
- **Current friction:** Rule select clipped “Percentage char…”.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Select sized to its longest option.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Unknown barcodes

Route: `/admin/unknown-barcodes` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Barcodes the till did not know.
- **Primary user:** Manager
- **Primary task:** Assign / create / dismiss
- **Primary action:** Resolve
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “Unknown Barcodes” → “Unknown barcodes”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Sentence case (nav, title, e2e).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Inventory

Route: `/admin/inventory` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Stock levels.
- **Primary user:** Stock keeper
- **Primary task:** Find low stock / adjust
- **Primary action:** Adjust stock
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “Total SKUs” (jargon), Title Case filter chips.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** “All products”, “Low stock”, “Out of stock”.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Stock movements

Route: `/admin/movements` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Every stock change.
- **Primary user:** Manager
- **Primary task:** Trace a change
- **Primary action:** Filter
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** Subtitle “The append-only ledger…”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** “Every change to stock, newest first: sales, refunds, deliveries, counts and adjustments. Entries are never edited or deleted.”
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Stocktake

Route: `/admin/stocktake` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Count and reconcile.
- **Primary user:** Stock keeper
- **Primary task:** Count
- **Primary action:** New stocktake
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “New Stocktake” button vs “New stocktake” dialog.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** One spelling.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Locations & transfers

Route: `/admin/transfers` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Locations and stock moves between them.
- **Primary user:** Stock keeper
- **Primary task:** Move stock
- **Primary action:** New transfer
- **Current friction:** None found.
- **First-time ambiguity:** Empty list said only “No transfers”.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Empty state explains ship-then-receive and when counts change.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Suppliers

Route: `/admin/suppliers` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Supplier list.
- **Primary user:** Buyer
- **Primary task:** Add / open supplier
- **Primary action:** Add supplier
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “+ Supplier”, column “Open POs”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** “Add supplier”, “Open orders”.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Supplier record

Route: `/admin/suppliers/:id` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Supplier details and purchase orders.
- **Primary user:** Buyer
- **Primary task:** Edit / see orders
- **Primary action:** Edit
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** Column “Purchase Orders”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Sentence case; name heading `dir=auto`.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Purchase orders

Route: `/admin/purchase-orders` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Orders to suppliers.
- **Primary user:** Buyer
- **Primary task:** Create / receive
- **Primary action:** New purchase order
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “+ Purchase Order”, column “PO”, raw status codes in Arabic.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** “New purchase order”, “Order no.”, translated statuses.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Receiving

Route: `/admin/receiving` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Book delivered goods into stock.
- **Primary user:** Stock keeper
- **Primary task:** Receive goods
- **Primary action:** Receive goods
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “Receive Goods”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Sentence case.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Payables

Route: `/admin/payables` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** What you owe suppliers; review, post, pay.
- **Primary user:** Owner
- **Primary task:** Post and pay invoices
- **Primary action:** Add supplier invoice
- **Current friction:** Voiding an unposted invoice was only possible from a second, duplicate list under Supplier documents.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Void (with confirmation) added to the Payables invoice drawer for unposted records; the duplicate list was removed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory); `e2e/whatsapp_documents.spec.ts` payables flow now voids an unposted duplicate and checks the balance is unchanged

### Supplier documents

Route: `/admin/invoice-scan` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Read supplier paperwork into drafts.
- **Primary user:** Buyer
- **Primary task:** Upload and check
- **Primary action:** Add document
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Had a third tab “Supplier invoices” duplicating Payables → two places for one record.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Tab removed; a line links to Payables, the single home for supplier invoices.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Document review

Route: `/admin/invoice-scan/:id` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Check what OCR read against the paper.
- **Primary user:** Buyer
- **Primary task:** Correct and accept
- **Primary action:** Create drafts
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/whatsapp_documents.spec.ts`; Rust docintel tests

### Reports

Route: `/admin/reports` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Report catalogue.
- **Primary user:** Owner / accountant
- **Primary task:** Open a report
- **Primary action:** Open
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** Report groups shown in English in Arabic.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Groups and titles translated.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Report view

Route: `/admin/reports/:key` — **DEFECT FIXED**

- **Purpose:** One report with a date range and export.
- **Primary user:** Owner / accountant
- **Primary task:** Read / export
- **Primary action:** Export
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** A failed report showed a skeleton forever beside the error.
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Skeleton removed on error; title translated.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Analytics

Route: `/admin/analytics` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Trends.
- **Primary user:** Owner
- **Primary task:** Read trends
- **Primary action:** Change range
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### End of day

Route: `/admin/end-of-day` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Close the day.
- **Primary user:** Manager
- **Primary task:** Check the day
- **Primary action:** Print / share
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Phone view

Route: `/admin/phone-view` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Pair a phone for read-only figures.
- **Primary user:** Owner
- **Primary task:** Pair phone
- **Primary action:** Show code
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### WhatsApp

Route: `/admin/whatsapp` — **EXTERNAL/HARDWARE VERIFICATION REQUIRED**

- **Purpose:** Link a number; conversations, triage, templates, catalogue.
- **Primary user:** Owner
- **Primary task:** Link phone
- **Primary action:** Link and show QR code
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; a real WhatsApp account and phone are needed for acceptance

### AI Assistant

Route: `/admin/ai` — **DEFECT FIXED**

- **Purpose:** Ask about the business; propose changes for approval.
- **Primary user:** Owner / manager
- **Primary task:** Ask
- **Primary action:** Send
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** Language chip read “UI”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** No page heading.
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Screen-reader `h1`; chip reads “Same as screen” / “English” / “العربية”.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Users & roles — Users

Route: `/admin/users` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Staff and their PINs.
- **Primary user:** Owner
- **Primary task:** Add / lock / unlock
- **Primary action:** Add user
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “+ User”, “Last Login”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** “Add user”, “Last sign-in”.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Roles & Permissions

Route: `/admin/roles` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** What each role may do.
- **Primary user:** Owner
- **Primary task:** Edit a role
- **Primary action:** Save role
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** Permission codes and English descriptions shown in Arabic; “+ Role”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Translated names/groups/descriptions, code kept as tooltip; “New role”; breadcrumb names the page.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### My profile

Route: `/admin/profile` — **DEFECT FIXED**

- **Purpose:** Your sign-in and PIN.
- **Primary user:** Anyone
- **Primary task:** Change PIN
- **Primary action:** Change PIN
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** No heading; breadcrumb showed “…”.
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Page header and breadcrumb label.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Branches

Route: `/admin/branches` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Multi-branch setup.
- **Primary user:** Owner
- **Primary task:** Add branch
- **Primary action:** Add branch
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Devices

Route: `/admin/devices` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Registered tills.
- **Primary user:** Owner
- **Primary task:** Revoke a till
- **Primary action:** Revoke terminal
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** Mode column showed the raw code “standalone”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Translated label.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Sync / Hub

Route: `/admin/sync` — **EXTERNAL/HARDWARE VERIFICATION REQUIRED**

- **Purpose:** Hub mode and sync health.
- **Primary user:** Owner
- **Primary task:** Enable hub / check
- **Primary action:** Enable hub mode
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; multi-computer sync needs store hardware

### Import products

Route: `/admin/import` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** CSV import with review.
- **Primary user:** Owner
- **Primary task:** Import
- **Primary action:** Apply import
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “CSV import with explicit review… Apply Import”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** “Bring products in from a CSV file. You check every row first; nothing changes until you press Apply import.”
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Migration

Route: `/admin/migration` — **REVIEWED — NO CHANGE NEEDED**

- **Purpose:** Bring data from another system.
- **Primary user:** Owner
- **Primary task:** Upload files
- **Primary action:** Choose files
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Backups

Route: `/admin/backups` — **DEFECT FIXED**

- **Purpose:** Backups and restore.
- **Primary user:** Owner
- **Primary task:** Back up / restore
- **Primary action:** Back up now
- **Current friction:** Restore-path placeholder showed doubled backslashes (`E:\\AMWAPOS…`); type column showed raw “manual”; “Backup Now”.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Placeholder fixed; types translated (“Manual”, “Automatic”, “Other file”); “Back up now”.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Audit

Route: `/admin/audit` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Tamper-evident log.
- **Primary user:** Owner
- **Primary task:** Trace an action
- **Primary action:** Filter
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** Date inputs always shown.
- **Terminology:** Action codes (`auth.login`) are shown as codes on purpose: this is an investigation log and codes are what support asks for.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Date range progressive disclosure.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Settings

Route: `/admin/settings` — **DEFECT FIXED**

- **Purpose:** All store settings in 18 sections.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Links such as “Open Settings → Features” did not switch section when Settings was already open (the section was read only on first mount).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** The section now lives in the address (`?section=`), so every deep link opens it; a sweep check fails on the wrong section.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Diagnostics

Route: `/admin/diagnostics` — **IMPROVEMENT IMPLEMENTED**

- **Purpose:** Health checks and an export for support.
- **Primary user:** Owner / support
- **Primary task:** Check health
- **Primary action:** Run health check
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** “Run Health Check”, “Export Diagnostics”.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Sentence case.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; `e2e/surfaces.spec.ts` role sweep (manager, accountant, inventory)

### Updates

Route: `/admin/updates` — **EXTERNAL/HARDWARE VERIFICATION REQUIRED**

- **Purpose:** Check and install updates.
- **Primary user:** Owner
- **Primary task:** Update
- **Primary action:** Install
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Reached from the sidebar, Ctrl K palette and breadcrumb; the breadcrumb names the page.
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** No change needed.
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; a signed update feed and a Windows install are needed

## Settings sections

### Settings → Business

Route: `/admin/settings?section=business` — **DEFECT FIXED**

- **Purpose:** Name, CR/VAT numbers, address, currency (locked after first sale).
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=business` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Tax

Route: `/admin/settings?section=tax` — **DEFECT FIXED**

- **Purpose:** VAT rate and inclusive/exclusive pricing.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=tax` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → POS

Route: `/admin/settings?section=pos` — **DEFECT FIXED**

- **Purpose:** Till behaviour.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=pos` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Shifts & cash

Route: `/admin/settings?section=shift` — **DEFECT FIXED**

- **Purpose:** Float, variance and drop rules.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=shift` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Payments

Route: `/admin/settings?section=payments` — **DEFECT FIXED**

- **Purpose:** Tenders available at the till.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=payments` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Receipts

Route: `/admin/settings?section=receipt` — **DEFECT FIXED**

- **Purpose:** Receipt header/footer and languages.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=receipt` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Printers

Route: `/admin/settings?section=printer` — **EXTERNAL/HARDWARE VERIFICATION REQUIRED**

- **Purpose:** Receipt printer connection (needs hardware to accept).
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=printer` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Inventory

Route: `/admin/settings?section=inventory` — **DEFECT FIXED**

- **Purpose:** Low-stock rules.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=inventory` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Security

Route: `/admin/settings?section=security` — **DEFECT FIXED**

- **Purpose:** Lock timeout, PIN rules.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=security` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Backups

Route: `/admin/settings?section=backup` — **DEFECT FIXED**

- **Purpose:** Backup schedule and folder.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=backup` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Appearance

Route: `/admin/settings?section=appearance` — **DEFECT FIXED**

- **Purpose:** Theme and text size.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=appearance` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Features

Route: `/admin/settings?section=features` — **DEFECT FIXED**

- **Purpose:** Optional modules on/off.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=features` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Loyalty

Route: `/admin/settings?section=loyalty` — **DEFECT FIXED**

- **Purpose:** Points earn/redeem.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=loyalty` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Delivery

Route: `/admin/settings?section=delivery` — **DEFECT FIXED**

- **Purpose:** Zones and fees.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=delivery` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → Product images

Route: `/admin/settings?section=images` — **DEFECT FIXED**

- **Purpose:** Automatic product pictures.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=images` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → WhatsApp

Route: `/admin/settings?section=whatsapp` — **DEFECT FIXED**

- **Purpose:** Message templates.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=whatsapp` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → AI assistant

Route: `/admin/settings?section=ai` — **DEFECT FIXED**

- **Purpose:** Provider, keys, answer language.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=ai` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

### Settings → About

Route: `/admin/settings?section=about` — **DEFECT FIXED**

- **Purpose:** Version and licences.
- **Primary user:** Owner
- **Primary task:** Change a setting
- **Primary action:** Save changes
- **Current friction:** None found.
- **First-time ambiguity:** None found.
- **Unnecessary information / decisions:** None found.
- **Terminology:** None found.
- **Navigation:** Deep link `?section=about` now opens this section even when Settings is already open (was broken).
- **State / feedback:** Skeleton while loading (`aria-busy`), red banner with the backend's plain-language error, toast after a write; buttons show a spinner and are disabled while a write runs (no double submit).
- **Accessibility:** One `h1`; labelled controls; Drawer/Modal/Confirm trap focus, close on Esc and return focus (`common.tsx`, `ui.tsx`).
- **Responsive / touch:** Controls ≥ 44 px (tokens); no sideways scroll at 1024×768 in English or Arabic.
- **Recommended action:** Keep as is.
- **Implementation status:** Section follows the address (shared fix).
- **Verification evidence:** `e2e/surfaces.spec.ts` EN + AR at 1024×768 (heading, no error banner, no stuck skeleton, no sideways scroll, no script error, no English in the Arabic interface); screenshot reviewed by hand; sweep asserts the active section matches the link

## Dialogs, drawers and confirmations

Each inherits the fields of the screen it opens from; only what differs is noted.

| Area | Dialog / drawer / confirmation | Classification | Notes |
|---|---|---|---|
| Admin shell | Go to… (Ctrl K palette) | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| AI | Confirm proposal | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| AI | Shown once (key) | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| AI | Tool detail | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| AI | Edit/New briefing | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| AI | Keyboard shortcuts | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| AI | Model | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| AI | Attach | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| AI | Conversations | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| AI | Proposals and evidence | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Unlink phone | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Back up WhatsApp session | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Link or create customer | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | New ticket | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Ticket step confirm | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Upload screenshot | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Payment review drawer | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Add document | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Choose product | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Confirm order | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Cancel order | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Send reply | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Sync catalogue | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Import WhatsApp customers | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Order detail | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Cancel order (till) | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| WhatsApp & orders | Order confirm warning | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Catalogue | Archive/Restore products | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Catalogue | Discard unsaved changes? | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Catalogue | New/Edit category | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Catalogue | Archive category | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Catalogue | Apply price changes | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Catalogue | Resolve barcode | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Catalogue | Assign barcode | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Customers | New customer | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Customers | Edit customer | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Customers | Add/Edit address | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Customers | Take payment | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Customers | Adjust balance | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Customers | Adjust points | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Documents | Reject document | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Documents | Receiving draft | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Documents | Receive this stock | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Documents | Cancel draft | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Inventory | Adjust stock | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Inventory | New stocktake | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Inventory | Cancel this stocktake? | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Inventory | Finalize stocktake | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Inventory | Stock at location | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Inventory | Add/Edit location | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Inventory | New transfer | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Purchasing & payables | New/Edit supplier | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Purchasing & payables | Place / cancel purchase order | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Purchasing & payables | Supplier account | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Purchasing & payables | Pay supplier | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Purchasing & payables | Invoice drawer | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Purchasing & payables | Post invoice | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Purchasing & payables | Reverse invoice | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Purchasing & payables | Void invoice (new) | IMPROVEMENT IMPLEMENTED | Moved here from the removed duplicate list; backend still refuses posted invoices. |
| Purchasing & payables | Add supplier invoice | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Sales | Receipt drawer | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Sales | Shift drawer | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Staff | New/Edit user | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| System | Device drawer | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| System | Revoke terminal | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| System | Reset hub credentials | EXTERNAL/HARDWARE VERIFICATION REQUIRED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. Real restore/update/hub on store hardware is part of acceptance. |
| System | Enable hub mode | EXTERNAL/HARDWARE VERIFICATION REQUIRED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. Real restore/update/hub on store hardware is part of acceptance. |
| System | Restore backup | EXTERNAL/HARDWARE VERIFICATION REQUIRED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. Real restore/update/hub on store hardware is part of acceptance. |
| System | Audit entry drawer | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| System | Install update | EXTERNAL/HARDWARE VERIFICATION REQUIRED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. Real restore/update/hub on store hardware is part of acceptance. |
| System | Apply migration table | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Branches | Add/Edit branch | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |
| Branches | Branches for user | REVIEWED — NO CHANGE NEEDED | Uses shared Modal/Drawer/Confirm: title, focus trap, Esc, busy-disabled confirm, inline error; destructive ones use the red button and say what cannot be undone. |

## Cross-cutting review

### Information architecture
Admin is grouped by job (Orders & customers, Sales, Catalog, Inventory,
Purchasing, Business, Automation, System) with rarely used system tools behind
“More tools”. One duplicate path was found and removed: supplier invoices were
listed both under Supplier documents and in Payables. Payables is now their
only home (review, post, void, pay, reverse). Roles and My profile are not in
the sidebar; they are reached from Users and the avatar, and the breadcrumb now
names them.

### Clarity test (where am I / what for / what to do / what to ignore / what next)
Every Admin page has a breadcrumb and an `h1`; most have a one-line subtitle
saying what the page is for. Subtitles that answered with internal words
(“immutable”, “append-only ledger”, “explicit review”) were rewritten in plain
language. Empty states say what will appear and how (transfers was the one
that did not).

### Forms
Required fields are marked; validation is inline and repeated by the backend.
Unsaved product edits ask before leaving. Selects are sized to their longest
option (two were clipping).

### Tables
Shared `DataTable`: sortable headers, skeleton rows while loading, an empty
message, a pager whose page-size select now fits its label. Dates use no-break
spaces and codes (`.mono`) never wrap, so a row stays one line at 1024 px.

### Status language
Backend codes go through `codeLabel()`; two places showed raw codes
(Devices mode, Backups type) and now use it. A new label “Other file” covers
backups inspected from outside the backup folder. Stock and button labels are
sentence case throughout.

### Empty, loading, error, double submit
Loading: `Skeleton` with `aria-busy`; the sweep fails a page that never
finishes. Error: red banner with the backend's own sentence (translated by
`tb()`); the report page used to show both a skeleton and the error. Double
submit: every write goes through `useAction` (busy flag disables the button)
and money writes carry an operation id the backend de-duplicates.

### Destructive actions
All go through `Confirm` with a red button that names the action (“Void
record”, “Reverse”, “Revoke terminal”) and a sentence saying what cannot be
undone. Posted money is never deleted; it is reversed.

### Permissions (owner, manager, accountant, inventory, cashier, delivery)
The sidebar shows a page only when the role holds one of its permissions and
its module is on; the backend checks again on every command. The role sweep
signs in as manager, accountant and inventory and opens every link each is
shown. Cashier and delivery roles have no `admin.access`: they never see Admin
and work from the till (sale, refund within limits, delivery desk). Hiding a
link is never the protection: `e2e/checkout.spec.ts` and the Rust permission
tests call the commands directly.

### Arabic / RTL
The Arabic sweep found 118 English strings at the start of this pass (date
presets, purchase-order statuses, report groups and titles, role names,
permission groups and descriptions, “days”, “to”) and one user-data layout
issue (area and rider chips). All are fixed; the sweep now passes with zero
findings and fails on any regression. Names people type (products,
customers, suppliers, conversation titles, areas, riders) are `dir=auto`.
Known limit: in RTL a long barcode under a cart line truncates on its leading
digits (the full barcode is in the line's tooltip) — Low, deferred: changing
the ellipsis side needs a per-field direction rule that would also affect
Latin product names.

### Touch and display
1024×768 and 1024×700 (payment sheet), 125 % and 150 % scaling are covered by
`e2e/layout1024.spec.ts`; the Admin sweep checks every page for sideways
scroll at 1024×768 in both languages. Controls are 44 px or more (56–64 px on
the till).

### Accessibility
Page headings added where missing (till, setup steps, AI, Profile); duplicate
login heading removed; order-journey steps have full names; Settings sections
mark the current one with `aria-current=page`; date presets expose
`aria-pressed`; the Admin connection pill now has readable contrast (it was
light grey on white). Dialogs trap focus and close on Esc. Colour contrast of
the remaining tokens was checked in the earlier design pass (`docs/UI.md`).

### Visual system
New styles use tokens only (`--surface`, `--border`, `--text-2`,
`--danger`, `--danger-soft`). The inline `filter: invert(0)` hack around the
Admin pill was replaced with a class.

### Backend / frontend coherence found through UX
- Settings section lived only in component state, so links from other pages
  (FeatureGate, AI) could not open a section while Settings was mounted; it
  now lives in the URL.
- Admin mode was not part of the session, so a reload (and the language
  switch, which reloads) sent owners back to the till; it is now kept in the
  session store, cleared at sign-out, and only restored for a user who still
  holds `admin.access`.
- Voiding an unposted supplier invoice existed in the backend
  (`supplier_invoices.set_status`, refuses posted ones) but was reachable only
  from the duplicate list; it is now in Payables, guarded by the same
  permissions the backend checks.

## Intentionally deferred

| Item | Severity | Reason |
|---|---|---|
| Audit log shows action codes (`auth.login`) | Low | The log is an investigation tool; support and exports use the code. Translating 200+ codes would hide the key people search for. |
| RTL truncation side for barcodes under cart lines | Low | See Arabic section. |
| “Users & Roles” sidebar label vs “Users” page title | Low | The page carries a Roles & Permissions button; renaming the page would split one destination into two words for little gain. |

## External / hardware acceptance remaining

Receipt printer output and cash drawer kick; barcode scanner models; Windows
Hello unlock; hub + terminal sync across real computers and a real network
outage; WhatsApp linking with a real number; signed update install on
Windows; backup restore on the store computer; touch feel on the actual
panel. None of these can be proven from this repository.

## Regression protection

- `e2e/surfaces.spec.ts` — EN/AR sweep (now also Settings deep links) and the
  role sweep.
- `src/lib/__tests__/time.test.ts` — table dates never break.
- `e2e/checkout.spec.ts` — reload keeps Admin; renamed labels.
- i18n coverage tests keep every `t()` key translated.
