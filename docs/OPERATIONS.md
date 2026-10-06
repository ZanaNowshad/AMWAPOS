# Operations & acceptance

## Install

1. Run `AMWAPOS_<ver>_x64-setup.exe` as an administrator. It is a per-machine install, adds
   firewall rules and creates `%ProgramData%\AMWAPOS\data`.
2. First launch opens the setup wizard with three choices:
   - **New store:** enter the business (name, VAT, CR), branch, VAT rate, owner PIN, till code,
     receipt text, printer and backup folder.
   - **Join a hub:** enter the hub address and the pairing code from the hub's Admin →
     Sync / Hub.
3. Upgrades install over the top. Data is never touched by install, upgrade or uninstall, and a
   safety backup is taken before any schema migration.

## Files

| Path | Content |
| --- | --- |
| `%ProgramData%\AMWAPOS\data\amwapos.db` (+ `-wal`, `-shm`) | The store database |
| `%ProgramData%\AMWAPOS\data\backups\` | Scheduled and manual backups (`*.amwbak` + `.amwbak.json` manifest) |
| `%ProgramData%\AMWAPOS\data\backups\safety\` | Automatic backups before restore/migration |
| `%ProgramData%\AMWAPOS\logs\amwapos.YYYY-MM-DD.log` | JSON logs, one file per day, last 30 kept (level via `AMWAPOS_LOG`) |

## Daily routine

- **Open:** log in, count the float, then Open Shift.
- **Close:** More → Close shift. Count the cash; the expected amount stays hidden until counted if
  blind close is on. A variance above the limit needs a manager.
- Check backups every morning. When a backup is overdue or failed, every Admin page shows a red
  banner with **Backup Now**, and the till header shows a red **Backup overdue** pill (managers
  can back up from it). Cashiers who see the pill fetch a manager.

## Language (English / العربية)

- The language is chosen per computer: **العربية / English** on the login screen, the setup wizard,
  the Admin top bar, or the till's **More** menu. The screen reloads and the signed-in user stays
  signed in.
- In Arabic the whole screen is right-to-left. Amounts, quantities, barcodes and receipt numbers
  stay left-to-right (`BHD 18.450`), digits stay Western (0–9) as on receipts.
- Receipt language is separate: Settings → Receipts → **Receipt language** (English labels, or
  English / Arabic labels). Arabic text on receipts (store name, product Arabic names, header and
  footer lines, customer names) always prints, as an image line. Use **Test Print** after
  setting up a printer: the test page ends with an Arabic line that must print joined and
  right-to-left.

## Backup rule (operational requirement)

**AMWAPOS takes automatic backups only while it is running. There is no Windows service or
scheduled task.** So:

1. **The hub PC must keep AMWAPOS running during trading hours, and must be opened at least
   once in every backup interval (24 h by default).** Leaving it running overnight is recommended. Locking the
   screen is fine; signing out of Windows or shutting down stops backups.
2. When AMWAPOS starts and a backup is overdue, it takes one within about 60 seconds. A PC that
   was off overnight therefore catches up soon after opening.
3. Default schedule: every 24 h, keeping the last 14. Set an external/USB or network folder in
   Admin → Backups. A backup kept only on the same disk does not survive a disk failure.
4. The Dashboard and Diagnostics show **Backup: warning** when there has been no successful backup
   for twice the interval (48 h by default), or none at all. Treat that as a stop-the-day issue:
   press **Backup Now** and find out why the schedule missed.
5. Each terminal keeps its own local database and takes its own backups while open. The hub
   backup is the one that holds every till's synced sales.

A Windows scheduled task or service is deliberately **not** used. It would open the database
from a second process under another Windows account, which conflicts with the per-user credential
store and the data-folder ACLs, and it cannot be verified without Windows hardware. This will be
revisited after the Windows soak.

## Voids, expenses and petty cash

- **Rung up by mistake?** Go to More → Recent sales → the sale → **Void sale**, and pick a reason.
  This works only for today's sales in the open shift, with nothing refunded or sent for
  delivery; anything else is a refund. A cashier needs a manager's approval, which covers only
  that void.
- **Trading past midnight?** Set Settings → Shift → **Trading day ends at** (up to 06:00) so
  late sales count for the day that opened.
- **Bills** (Admin → Business → Expenses, on the hub):
  1. Add the expense and attach the bill.
  2. It is approved on entry for approvers, or waits to be approved.
  3. Record the payment. If the money came from the till, record a **Paid out** at the till first,
     then pick **Till paid-out** and that paid-out.
- **Petty cash:** top up the fund, count it weekly (**Count**), and explain any adjustment. A fund
  closes only at zero.
- **Monthly:** run Reports → **Operating profit**, and Reports → **Receivables** for customers who
  are late. Send each late customer their statement (Customer → Account → PDF or WhatsApp).

## Opening and closing the trading day

On the hub computer (or the only computer), go to Admin → Business → End of day.

- **Opening:** look at the top card. "Ready to trade" means nothing is
  waiting. Otherwise it lists:
  - yesterday not closed;
  - cash differences still open;
  - a backup that is due;
  - sync problems;
  - a computer that is not a register;
  - no shift open;
  - failed printing.

  Selling never waits for these.
- **During the day:** **View current totals** (X) as often as you like. It
  changes nothing.
- **Closing:**
  1. Every till counts its drawer and closes its shift. A shift still open
     on this computer must be closed first. One open on another till can
     wait; its cash counts in a later close.
  2. Read **Check these** and tick "I have read the items above".
  3. Press **Close trading day**.

  The close is permanent. Download its PDF from Closed days.
- **Days close in order.** If you missed a day, close it first; the
  message names it.
- **Late sales:** a till that was offline may send sales after its day was
  closed. They appear under **After-close adjustments** and are counted
  once in the next close. The closed day stays as it was.
- **Cash differences:** a drawer that differs by more than Settings →
  Shift → "Cash difference that opens a case" appears in Admin → Business
  → Cases. Mark it as seen, look into it, add notes or photos, then
  resolve it with what was found.
- **New computer replacing a till:** System → More tools → Registers.
  Open the till's register and choose the new computer.

## Batches, expiry and waste

- **Products with dates:** in Product → Inventory, tick "Ask for batch and
  expiry when receiving" and choose what the pack date means (expiry or
  best before).
- **Receiving:** type the batch code and expiry on each line. On a draft
  made from a supplier document, a date the reader found shows "Read from
  the document": check it against the pack, then press **Confirm date** or
  change it. Receiving waits until you do.
- **Every morning:** go to Inventory → Expiry and look at Expired and
  Urgent. Expired stock stays on the shelf in the system until you record
  what happened. To throw it away, press **Record waste** with the reason
  Expired. To sell it off, change the price on the product: the batch shows
  price options, but nothing changes by itself.
- **Waste:** record it from Inventory → Waste or from a batch. Choose the
  plain reason. Use "Shrinkage / unexplained difference" only when nobody
  knows why stock is missing; a manager confirms it, and confirms anything
  above the value in Settings → Inventory. A mistake is reversed (Waste →
  Reverse), never deleted.
- **Stock from before batches:** it shows as "Not in a batch" and sells
  first. If you read its date off the pack, use Product → Inventory →
  **Count stock into a batch**. Stock on hand does not change.
- **Days of stock left** (Inventory): how long each product lasts at the
  recent selling rate. "Not enough recent sales" means the product is too
  new to say.

## Ordering from suppliers (Wave 4)

Admin → Purchasing, on the hub computer. Details: [PROCUREMENT.md](PROCUREMENT.md).

- **Once per supplier:** Suppliers → (supplier) → **Ordering terms**: for each product, the
  units in one pack, the minimum order (packs), the lead time (days) and whether this is the
  preferred supplier. These are what Suggested orders use; what you type here wins over what
  supplier documents say.
- **Each order day:** **Suggested orders** → "To order". Each row says why (open it for the
  figures). Tick the rows and press **Create requisition**. Rows under "Needs a decision" need a
  supplier or a lead time first. Nothing is ordered from this screen.
- **Requisition:** check quantities, enter a cost where there is none, **Submit**. Someone who
  approves purchasing presses **Approve** (or **Reject**, with a reason), then **Create purchase
  orders**: one draft per supplier.
- **Purchase order:** if approval is on (Settings → Purchasing), the draft shows "Needs
  approval"; an approver presses **Approve**. Changing the supplier, lines, quantities, costs or
  taxes afterwards needs a new approval. Then **Place Order**.
- **Delivery:** open the order → **Receive goods**. Type what you accept, what you refuse (with
  the reason; it goes back with the driver and is not stock or waste), what you keep although
  damaged, and the batch and expiry. If something is missing, choose **Keep on order** or
  **Cancel the rest**. If more came than ordered, tick **Keep the extra**; beyond the tolerance a
  manager approves. If another product came instead, press **Another product came instead** and
  accept the substitute. Cost differences do not stop the delivery.
- **Supplier invoice:** in Payables, the invoice drawer shows **Order, goods received and
  invoice**. "Blocked" means it charges for more than was received: receive the goods or ask
  for a corrected invoice. "Needs review": an approver checks the differences and presses
  **Accept the differences** with a reason before it is posted.
- **Returning goods:** **Supplier returns** → **New return**: supplier, products (and the
  batch), quantity, reason → **Save Draft** → **Confirm: goods leave stock**. When the
  supplier's credit note comes, press **Record the credit note** and post it in Payables. A
  return confirmed by mistake is reversed, never deleted.

## Paying suppliers (Payables)

Admin → Purchasing → Payables, on the hub computer.
1. A supplier invoice arrives: scan it (Supplier documents) or press **Add supplier invoice**.
2. Check it against the paper and press **Mark reviewed**.
3. Press **Post**: only posted invoices count as owed. A posted invoice is never edited; a
   mistake is undone with **Reverse** (after removing payments applied to it).
4. To pay, open the supplier and press **Record payment**. The amount is applied to the oldest
   invoices first; change the amounts if the supplier was paid for specific invoices. What is
   not applied stays on the supplier's account.
The Dashboard lists overdue supplier invoices and invoices ready to post.
A record that was never posted (a duplicate, a wrong scan) is removed from the list with **Void
record** in its drawer; a posted one is undone with **Reverse** instead. Supplier documents only
reads paper into drafts; every supplier invoice is reviewed, posted, voided and paid here.

## Restore

Admin → Backups → Restore does the following:
1. Checks the manifest hash and the database integrity.
2. Takes a safety backup of the current data.
3. Restores and runs migrations.
4. Signs everyone out.

Restoring the **hub** to an older backup changes its identity, so terminals block sync until an
owner accepts the new hub. Unsynced terminal sales are kept and upload after that.

## Multi-terminal troubleshooting

| Symptom | Check |
| --- | --- |
| "This hub requires AMWAPOS sync protocol 2" / "needs protocol 2" | The hub and the terminal run different AMWAPOS versions. Install the same version on both. |
| Pairing: "No pairing code is active" or "no longer valid" | Only the newest code works, for 15 minutes, and five wrong entries cancel it. Generate a new code and pair one terminal at a time. |
| Till pill says **Update needed** | The hub and this till run different AMWAPOS versions (the hub answered HTTP 426 / another sync protocol). Install the same version on both; selling continues meanwhile. |
| Till pill says **Hub unreachable** | Network: see the next row. |
| Till pill says **Pair again** | The till was revoked, or its hub credential is missing. Pair it again from the hub. |
| Terminal shows "Offline" | Is the hub PC on? Can the terminal reach `http://<hub>:47800/health`? Is the network profile *Private*? |
| "Hub credential is missing" on the hub | AMWAPOS was started under a different Windows account. Sign in with the original account, or reset hub credentials and pair all terminals again. |
| "This hub is not the one this terminal paired with" | The hub was rebuilt or restored. Decide in Admin → Sync / Hub on the terminal. |
| Items in "Changes that could not be applied" | Read the problem column. Fix the cause (e.g. a missing product on the hub), then Retry. |

## Acceptance checklist (run on real Windows hardware)

Automated coverage exists for everything marked ✅. The ☐ items need a person with the hardware.

- ✅ Setup → login → shift → scan / search / unknown barcode → cash with change → split tender → refund → shift close with the expected cash (Playwright).
- ✅ A cashier cannot open Admin. An over-limit discount needs the manager's PIN, and a wrong PIN changes nothing. The approval is audited (Playwright).
- ✅ The same sale submitted twice produces one sale. Changed payloads are rejected (core tests).
- ✅ Two terminals sell offline and then sync with no duplicates, and stock converges. A lost push response is safe (sync tests). The encrypted HTTP hub round-trip works. No sensitive bytes appear on the wire. Protocol 1 is refused. Wrong codes cancel the pairing code (hub tests).
- ✅ Backup → restore round-trip; a tampered backup is refused (core tests). A new store shows the backup banner; Backup Now clears it (Playwright).
- ✅ Scanner burst: five scans at scanner speed with no waits, none dropped (Playwright).
- ✅ Arabic: language toggle, right-to-left sale, admin, dark/compact theme (Playwright). Every UI string has an Arabic translation (unit test).
- ✅ Arabic receipt lines are shaped and rasterized; no `?` reaches the printer; drawer pulses on cash sales only (printing tests).
- ✅ A hub on another version is reported as "Update needed", not offline (hub tests).
- ✅ 100k products: P95 scan 0.73 ms, search 23.6 ms, cart 0.78 ms, sale commit 9.9 ms (perf test, release build, Linux sandbox; targets 50 / 150 / 100 / 500 ms).
- ☐ Install, upgrade and uninstall on a clean Windows 10 and 11 VM. Data survives uninstall and firewall rules exist.
- ☐ USB/HID barcode scanner at full speed (13-digit EAN, Code128).
- ☐ 80 mm ESC/POS printer via network and via the Windows spooler. The cash drawer kicks on cash sales only. A paper-out failure keeps the sale.
- ☐ Arabic on paper: Test Print, then a sale of a product with an Arabic name and a bilingual receipt. Arabic is joined, right-to-left, not `?`.
- ☐ Two physical tills plus a hub over store Wi-Fi. Unplug the hub mid-sale and re-plug it.
- ☐ Power loss during a sale (pull the plug), then restart. The database is intact and the sale is either fully present or absent.
- ☐ Hub left running overnight: next morning the Dashboard shows a backup from the last 24 h. Hub shut down overnight: a backup appears within about 1 minute of opening AMWAPOS.

## Soak checklist (owner, before going live)

Read each line and tick it on the store computer. Nothing here is automatic.

- ☐ **Optional modules are off by default.** Settings → Features lists every module with "Default: off". Turn on only what the store uses.
- ☐ **AI on the till needs `ai.use`.** Cashiers do not get it from the upgrade. Grant it in Users → Roles before expecting the till Assistant to appear.
- ☐ **OpenRouter fallback is off.** It runs only after the owner ticks it and stores an OpenRouter key. It never runs for a wrong key (401) or a refusal.
- ☐ **Scheduled briefings and AI alerts run only while AMWAPOS is open** on the hub. There is no Windows service or Task Scheduler job.
- ☐ **The phone companion page needs the hub running and a live link token.** Revoke links you no longer use. The page is on the store network only (no public HTTPS).
- ☐ **The installer is unsigned** until a code-signing certificate is bought. Windows SmartScreen will warn. Check the SHA-256 from STATUS/CI before running it.
- ☐ **WhatsApp uses an unofficial connection.** WhatsApp can ban the number. Use a spare business number, not the owner's personal one.
- ☐ **A payment screenshot is not a settlement.** Check the BenefitPay/bank statement before marking an order paid. The review screen is a helper only.
- ☐ **Credit, loyalty, digital orders, multi-branch and the phone companion (PWA) stay off** until the owner turns each one on and trains staff.
- ☐ **AI dual control (`ai.dual_control`) is off.** Turn it on if high-risk AI proposals must be confirmed by a second person.
- ☐ **Product pictures look right.** The default source asks Bing for one picture per barcode + name. Open a few new products and replace any wrong picture by uploading one.
- ☐ **Delivery riders get `orders.manage`** with the Delivery role (to move digital orders to "out for delivery"). Remove it in Users → Roles if riders should not see orders.
