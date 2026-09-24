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
| `%ProgramData%\AMWAPOS\logs\amwapos.log.YYYY-MM-DD` | JSON logs (level via `AMWAPOS_LOG`) |

## Daily routine

- **Open:** log in, count the float, then Open Shift.
- **Close:** More → Close shift. Count the cash; the expected amount stays hidden until counted if
  blind close is on. A variance above the limit needs a manager.
- Backups run automatically, every 24 h by default, keeping 14. Set an external/USB or network
  folder in Admin → Backups and check the "last success" time there. The Dashboard warns when
  backups are overdue.

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
| Terminal shows "Offline" | Is the hub PC on? Can the terminal reach `http://<hub>:47800/health`? Is the network profile *Private*? |
| "Hub credential is missing" on the hub | AMWAPOS was started under a different Windows account. Sign in with the original account, or reset hub credentials and pair all terminals again. |
| "This hub is not the one this terminal paired with" | The hub was rebuilt or restored. Decide in Admin → Sync / Hub on the terminal. |
| Items in "Changes that could not be applied" | Read the problem column. Fix the cause (e.g. a missing product on the hub), then Retry. |

## Acceptance checklist (run on real Windows hardware)

Automated coverage exists for everything marked ✅. The ☐ items need a person with the hardware.

- ✅ Setup → login → shift → scan / search / unknown barcode → cash with change → split tender → refund → shift close with the expected cash (Playwright).
- ✅ A cashier cannot open Admin. An over-limit discount needs the manager's PIN, and a wrong PIN changes nothing. The approval is audited (Playwright).
- ✅ The same sale submitted twice produces one sale. Changed payloads are rejected (core tests).
- ✅ Two terminals sell offline and then sync with no duplicates, and stock converges. A lost push response is safe (sync tests). The signed HTTP hub round-trip works (hub test).
- ✅ Backup → restore round-trip; a tampered backup is refused (core tests).
- ✅ 100k products: P95 scan 0.56 ms, search 14.9 ms, sale commit 5.6 ms (perf test, release build).
- ☐ Install, upgrade and uninstall on a clean Windows 10 and 11 VM. Data survives uninstall and firewall rules exist.
- ☐ USB/HID barcode scanner at full speed (13-digit EAN, Code128).
- ☐ 80 mm ESC/POS printer via network and via the Windows spooler. The cash drawer kicks. A paper-out failure keeps the sale.
- ☐ Two physical tills plus a hub over store Wi-Fi. Unplug the hub mid-sale and re-plug it.
- ☐ Power loss during a sale (pull the plug), then restart. The database is intact and the sale is either fully present or absent.
