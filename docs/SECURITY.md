# Security

## Threat model (summary)

| Threat | Control |
| --- | --- |
| A cashier gives unauthorised discounts, voids, refunds or price changes | Role permissions enforced in the backend. Over-limit actions need a single-use manager approval token bound to the permission (120 s). Both people are recorded in the hash-chained audit log. |
| Someone guesses a staff PIN | Argon2id hashes; weak PINs (sequences, repeats) rejected; lockout after N failures (default 5 → 15 min). |
| Someone tampers with history in the database file | Append-only triggers on financial tables. SHA-256 audit chain whose verification shows the first broken link. Checksummed migrations. |
| A device on the store LAN impersonates a terminal | Pairing needs a single-use 8-digit code shown on the hub (hashed at rest, 15 min, 5 attempts per 5 min per IP). All later traffic is HMAC-signed per device, with nonce replay protection, a ±5 min clock window and signed responses. A revoked device is refused. |
| A terminal pushes rows that belong to another device | The hub checks the device owns every append-only row it receives. Hub-owned tables are never accepted from terminals. |
| A rolled-back or rebuilt hub corrupts terminals | `hub_instance_id` pinning; the terminal blocks sync until an owner decides. |
| A replayed or double-submitted payment | Idempotent operations commit in one transaction; retries return the original result. |
| A malicious CSV import | 60 MB / 200k-row limits, scientific-notation detection, validated preview, all-or-nothing idempotent apply. |
| Formula injection via CSV exports opened in Excel | Text cells starting with `= + - @` TAB or CR are prefixed with `'`; the importer strips that prefix again. |
| XSS in the webview | Strict CSP (no remote scripts, no `unsafe-eval`). React escaping. Tauri capabilities limited to `core:default` plus one `rpc` command. |

## Secrets

- The hub master secret and the terminal device key are stored in Windows Credential Manager
  (`keyring`, service `AMWAPOS`). The database holds only a SHA-256 fingerprint of the hub
  secret.
- Credential Manager entries belong to one Windows account. Run AMWAPOS under a single Windows
  account per till. If the hub is started under a different account it reports the missing
  credential. It does not generate a new one, which would silently break every paired terminal.
  **Admin → Sync / Hub → Reset hub credentials** replaces it; every terminal must then pair again.
- Staff sessions exist only in memory. They are never written to disk or logs.
- The dev bridge (`amwapos-devserver`) keeps secrets in a plain file. It binds to loopback only
  and is not shipped.

## Data at rest

- `%ProgramData%\AMWAPOS` is created by the installer with explicit ACLs:
  - SYSTEM and Administrators have full control.
  - local Users can modify.
- The SQLite database is **not encrypted**. Anyone with Windows access to the till can copy the
  file. Protect the Windows accounts and enable BitLocker on tills and the hub.
- Backups carry a SHA-256 manifest and are verified (`PRAGMA integrity_check`) after creation and
  before restore. Store off-site copies securely.

## Network

- The hub listens on TCP 47800 and UDP 47801 discovery. The installer's firewall rules allow them
  on **private** profiles only, bound to `amwapos.exe`.
- Traffic is authenticated and integrity-protected (HMAC) but **not encrypted** (no TLS). Catalogue,
  sales and customer data cross the store LAN in clear text. Keep the POS network separate from
  guest Wi-Fi.

## Reporting

Report vulnerabilities privately to the repository owner. Do not open public issues for them.
