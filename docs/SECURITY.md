# Security

## Threat model (summary)

| Threat | Control |
| --- | --- |
| A cashier gives unauthorised discounts, voids, refunds or price changes | Role permissions enforced in the backend. Over-limit actions need a single-use manager approval token bound to the permission (120 s). Both people are recorded in the hash-chained audit log. |
| Someone guesses a staff PIN | Argon2id hashes; weak PINs (sequences, repeats) rejected; lockout after N failures (default 5 → 15 min). |
| Someone tampers with history in the database file | Append-only triggers on financial tables. SHA-256 audit chain whose verification shows the first broken link. Checksummed migrations. |
| Someone on the store LAN or Wi-Fi reads sync traffic | Sync protocol 2 encrypts every pairing and sync body (ChaCha20-Poly1305). A test runs a full pairing, sale and sync through a recording proxy and checks that no product name, barcode, receipt number, pairing code, device key or PIN hash appears on the wire. |
| A device on the store LAN impersonates a terminal or the hub | Pairing uses SPAKE2 with the single-use 8-digit code, which is never sent. An eavesdropper cannot test guesses offline, and each online guess needs a full exchange. Limits: one active code; 15 min; 5 attempts per IP per 5 min; 5 wrong codes cancel the code. Later traffic is sealed and HMAC-signed with a per-device key, with nonce replay protection and a ±5 min clock window. Replies are sealed and signed too. A revoked device is refused. |
| An old or modified client talks plaintext to the hub | The hub accepts only sync protocol 2. Protocol 1 and requests without the protocol header get HTTP 426. |
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

## Residual risks (accepted, documented)

These are known and deliberate. None of them is silently weakened by the code.

| Risk | Why it remains | Mitigation |
| --- | --- | --- |
| HTTP headers are readable on the LAN: route, device id, timestamp, nonce, signature, protocol version | The channel encrypts bodies, not HTTP framing | No business data is in headers; the device id alone grants nothing (every request needs the device key) |
| Message sizes and timing are visible | Encryption does not hide length or when a till syncs | Reveals activity level, not content |
| `/health` and `/info` are public: product, version, business and hub name, hub instance id, outbox position | A till must find and identify a hub before pairing | No secrets; a till reads `/info` only when probing an address during setup; signed status comes from `/sync/status` |
| UDP discovery broadcasts the hub name and address | Tills find the hub without typing an IP | Private-profile firewall rule; pairing still needs the code |
| No forward secrecy for synced traffic | Traffic keys derive from each device key, which derives from the hub master secret | A recording is only readable with the hub's Windows credential; reset hub credentials after a suspected compromise (pairing exchanges use fresh SPAKE2 keys) |
| Database and backups are not encrypted at rest | SQLite with no page encryption | BitLocker on tills and hub; encrypted drives or access-controlled shares for backups (below) |
| Hub/till secrets are per Windows account | Windows Credential Manager | Run AMWAPOS under one Windows account per computer; a missing credential is reported, never silently replaced |
| Protocol downgrade | — | Not possible: the hub answers only protocol 2 (426 otherwise) and tills refuse a hub that reports another protocol; there is no plaintext fallback |

## What BitLocker covers, and what a stolen database yields

**Stolen database.** Someone who copies `amwapos.db`, or an unencrypted backup, can read
everything the store records:
- sales, refunds, payments and cash history;
- products, costs and suppliers;
- customer names and phone numbers;
- staff names and roles, and the audit log.

They also get each staff member's Argon2id PIN hash. Short PINs can be brute-forced offline: a
4-digit PIN in seconds, an 8-digit PIN in days to weeks on one computer. Treat PINs as exposed
after a theft and reset them.

The file does **not** contain the hub master secret, a terminal's device key or any session.
Those live in Windows Credential Manager or in memory, so a copied database alone cannot sync as
a terminal or impersonate the hub. Protecting the file therefore comes down to disk encryption
and Windows account control.

**BitLocker** protects a PC or drive that is taken while powered off. It does not protect:
- a running, unlocked till;
- anyone who can sign in to Windows on it (local Users can read the data folder);
- malware running on the PC;
- backups copied to USB drives or network shares.

Store those backups encrypted, for example on a BitLocker To Go drive or an access-controlled
share.

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
- **Encrypted (sync protocol 2):** everything a terminal sends or receives after `/info`:
  - pairing request and reply, including the device key and the bootstrap snapshot of catalogue,
    users with PIN hashes, and recent sales;
  - pushes, pulls, heartbeats, status and device lists.
- **In clear:**
  - HTTP headers: route, device id, timestamp, nonce, signature, protocol version;
  - message sizes and timing;
  - the public `/health` and `/info` replies (product, version, business name, hub name, hub
    instance id, outbox position), which a terminal fetches only when probing a hub address
    during setup;
  - UDP discovery broadcasts (hub name and address).
- **Why not TLS:** the channel is application-layer encryption, not TLS. That avoids distributing
  and renewing certificates on shop PCs. Trust comes from the pairing code, through a
  password-authenticated key exchange, not from a certificate authority.
- **Forward secrecy:** each paired device's traffic keys come from its device key. Someone who
  records traffic and later steals a hub's Windows credential (the master secret) could decrypt
  that recording. Pairing exchanges use fresh SPAKE2 keys each time.

## Reporting

Report vulnerabilities privately to the repository owner. Do not open public issues for them.
