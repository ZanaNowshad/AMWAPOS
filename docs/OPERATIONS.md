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
