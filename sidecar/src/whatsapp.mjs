// WhatsApp link through a linked device (QR pairing). States:
//   stopped → starting → pairing (QR shown) → connecting → connected → ready
//   logged_out (unlinked from the phone) | error
// "connected" means the socket is open; "ready" means the offline backlog has
// been received and messages can be sent.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import QRCode from "qrcode";
import makeWASocket, {
  Browsers,
  DisconnectReason,
  downloadMediaMessage,
  fetchLatestBaileysVersion,
  jidNormalizedUser,
  useMultiFileAuthState,
} from "@whiskeysockets/baileys";
import { HttpError } from "./http.mjs";

const MEDIA_MAX = 10 * 1024 * 1024;
const MEDIA_TYPES = { "image/jpeg": "jpg", "image/png": "png", "image/webp": "webp", "application/pdf": "pdf" };
const READY_FALLBACK_MS = 15000;

export function toJid(phone) {
  const digits = String(phone ?? "").replace(/[^\d]/g, "");
  if (digits.length < 8 || digits.length > 15) {
    throw new HttpError(400, "bad_phone", "Phone number must include the country code (8 to 15 digits).");
  }
  return `${digits}@s.whatsapp.net`;
}

export class WhatsApp {
  constructor({ dataDir, allowedRoots, log }) {
    this.dir = path.join(dataDir, "whatsapp");
    this.authDir = path.join(this.dir, "auth");
    this.mediaDir = path.join(this.dir, "media");
    this.inboxFile = path.join(this.dir, "inbox.jsonl");
    this.sentFile = path.join(this.dir, "sent.json");
    fs.mkdirSync(this.mediaDir, { recursive: true });
    this.allowedRoots = allowedRoots.map((r) => fs.realpathSync(r));
    this.log = log;
    this.state = "stopped";
    this.qr = null;
    this.me = null;
    this.lastError = null;
    this.sock = null;
    this.wanted = false;
    this.retry = 0;
    this.inbox = this.loadInbox();
    this.seq = this.inbox.length ? this.inbox[this.inbox.length - 1].seq : 0;
    this.sent = this.loadSent();
  }

  loadInbox() {
    try {
      return fs
        .readFileSync(this.inboxFile, "utf8")
        .split("\n")
        .filter(Boolean)
        .map((l) => JSON.parse(l));
    } catch {
      return [];
    }
  }

  loadSent() {
    try {
      return JSON.parse(fs.readFileSync(this.sentFile, "utf8"));
    } catch {
      return {};
    }
  }

  saveSent() {
    const tmp = `${this.sentFile}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify(this.sent));
    fs.renameSync(tmp, this.sentFile);
  }

  linked() {
    return fs.existsSync(path.join(this.authDir, "creds.json"));
  }

  status() {
    return {
      state: this.state,
      connected: this.state === "connected" || this.state === "ready",
      ready: this.state === "ready",
      linked: this.linked(),
      qr_data_url: this.state === "pairing" ? this.qr : null,
      me: this.me,
      last_error: this.lastError,
      inbox_seq: this.seq,
    };
  }

  async start() {
    this.wanted = true;
    if (this.sock) return this.status();
    await this.connect();
    return this.status();
  }

  async connect() {
    this.state = "starting";
    this.lastError = null;
    fs.mkdirSync(this.authDir, { recursive: true });
    const { state, saveCreds } = await useMultiFileAuthState(this.authDir);
    let version;
    try {
      ({ version } = await fetchLatestBaileysVersion());
    } catch {
      version = undefined; // library default
    }
    const sock = makeWASocket({
      auth: state,
      version,
      browser: Browsers.windows("AMWAPOS"),
      logger: this.log.child({ mod: "baileys" }, { level: "warn" }),
      markOnlineOnConnect: false,
      syncFullHistory: false,
    });
    this.sock = sock;
    let readyTimer = null;
    sock.ev.on("creds.update", saveCreds);
    sock.ev.on("connection.update", async (u) => {
      if (this.sock !== sock) return;
      if (u.qr) {
        this.state = "pairing";
        this.qr = await QRCode.toDataURL(u.qr, { margin: 1, width: 320 });
      }
      if (u.connection === "connecting" && this.state !== "pairing") this.state = "connecting";
      if (u.connection === "open") {
        this.retry = 0;
        this.qr = null;
        this.state = "connected";
        this.me = sock.user ? { id: jidNormalizedUser(sock.user.id), name: sock.user.name ?? null } : null;
        readyTimer = setTimeout(() => {
          if (this.sock === sock && this.state === "connected") this.state = "ready";
        }, READY_FALLBACK_MS);
      }
      if (u.receivedPendingNotifications && this.state === "connected") this.state = "ready";
      if (u.connection === "close") {
        clearTimeout(readyTimer);
        this.sock = null;
        const code = u.lastDisconnect?.error?.output?.statusCode;
        if (code === DisconnectReason.loggedOut) {
          this.state = "logged_out";
          this.me = null;
          fs.rmSync(this.authDir, { recursive: true, force: true });
          return;
        }
        this.lastError = u.lastDisconnect?.error?.message ?? "Connection closed";
        if (!this.wanted) {
          this.state = "stopped";
          return;
        }
        this.state = "connecting";
        const delay = Math.min(60000, 2000 * 2 ** Math.min(this.retry++, 5));
        setTimeout(() => {
          if (this.wanted && !this.sock) this.connect().catch((e) => this.fail(e));
        }, delay);
      }
    });
    sock.ev.on("messages.upsert", ({ messages, type }) => {
      if (type !== "notify") return;
      for (const m of messages) this.receive(m).catch((e) => this.log.warn({ err: String(e) }, "inbound message dropped"));
    });
  }

  fail(e) {
    this.state = "error";
    this.lastError = String(e?.message ?? e);
  }

  async stop() {
    this.wanted = false;
    const s = this.sock;
    this.sock = null;
    if (s) s.end(undefined);
    this.state = "stopped";
    this.qr = null;
    return this.status();
  }

  async logout() {
    this.wanted = false;
    const s = this.sock;
    this.sock = null;
    if (s) await s.logout().catch(() => {});
    fs.rmSync(this.authDir, { recursive: true, force: true });
    this.state = "stopped";
    this.qr = null;
    this.me = null;
    return this.status();
  }

  async receive(m) {
    if (!m.message || m.key.fromMe) return;
    const chat = m.key.remoteJid ?? "";
    if (chat.endsWith("@g.us") || chat === "status@broadcast") return; // groups and statuses are ignored
    const msg = m.message.ephemeralMessage?.message ?? m.message;
    const rec = {
      id: m.key.id,
      chat,
      from: m.key.participant ?? chat,
      sender_pn: m.key.senderPn ?? null,
      push_name: m.pushName ?? null,
      ts: Number(m.messageTimestamp ?? Math.floor(Date.now() / 1000)),
      type: "other",
      text: null,
      caption: null,
      media: null,
    };
    if (msg.conversation || msg.extendedTextMessage) {
      rec.type = "text";
      rec.text = msg.conversation || msg.extendedTextMessage?.text || "";
    } else if (msg.imageMessage || msg.documentMessage) {
      const part = msg.imageMessage ?? msg.documentMessage;
      rec.type = msg.imageMessage ? "image" : "document";
      rec.caption = part.caption ?? null;
      const mime = (part.mimetype ?? "").split(";")[0];
      const size = Number(part.fileLength ?? 0);
      if (MEDIA_TYPES[mime] && size <= MEDIA_MAX) {
        const buf = await downloadMediaMessage(m, "buffer", {}, { logger: this.log, reuploadRequest: this.sock?.updateMediaMessage });
        if (buf.length <= MEDIA_MAX) {
          const sha256 = crypto.createHash("sha256").update(buf).digest("hex");
          const file = path.join(this.mediaDir, `${sha256}.${MEDIA_TYPES[mime]}`);
          fs.writeFileSync(file, buf);
          rec.media = { path: file, mime, size: buf.length, sha256, file_name: part.fileName ?? null };
        }
      }
    }
    if (this.inbox.some((x) => x.id === rec.id && x.chat === rec.chat)) return;
    rec.seq = ++this.seq;
    this.inbox.push(rec);
    fs.appendFileSync(this.inboxFile, JSON.stringify(rec) + "\n");
  }

  messages({ after, limit }) {
    const a = Number(after ?? 0) || 0;
    const n = Math.min(Math.max(Number(limit ?? 100) || 100, 1), 500);
    const out = this.inbox.filter((m) => m.seq > a).slice(0, n);
    return { messages: out, next: out.length ? out[out.length - 1].seq : a };
  }

  requireReady() {
    if (!this.sock || this.state !== "ready") {
      throw new HttpError(409, "not_ready", "WhatsApp is not connected and ready.", { state: this.state });
    }
    return this.sock;
  }

  resolveFile(p) {
    let real;
    try {
      real = fs.realpathSync(p);
    } catch {
      throw new HttpError(404, "not_found", "Document file not found.");
    }
    if (!this.allowedRoots.some((r) => real.startsWith(r + path.sep))) {
      throw new HttpError(403, "forbidden", "The sidecar only sends files inside the AMWAPOS data folder.");
    }
    return real;
  }

  /** Send once per client_id: a retried request returns the first result. */
  async send({ client_id, to, text, document }) {
    if (typeof client_id !== "string" || client_id.length < 8) throw new HttpError(400, "bad_request", "client_id is required.");
    if (this.sent[client_id]) return { ...this.sent[client_id], replay: true };
    const sock = this.requireReady();
    const jid = toJid(to);
    const [found] = await sock.onWhatsApp(jid);
    if (!found?.exists) throw new HttpError(404, "not_on_whatsapp", "This number is not on WhatsApp.");
    let content;
    if (document) {
      const file = this.resolveFile(document.path);
      content = {
        document: fs.readFileSync(file),
        mimetype: document.mime ?? "application/pdf",
        fileName: document.file_name ?? path.basename(file),
        caption: text ?? undefined,
      };
    } else {
      if (typeof text !== "string" || !text.trim()) throw new HttpError(400, "bad_request", "Message text is empty.");
      content = { text };
    }
    const r = await sock.sendMessage(found.jid ?? jid, content);
    const result = { message_id: r?.key?.id ?? null, jid: found.jid ?? jid, sent_at: new Date().toISOString() };
    this.sent[client_id] = result;
    this.saveSent();
    return result;
  }

  async markRead({ keys }) {
    const sock = this.requireReady();
    if (!Array.isArray(keys) || !keys.length) return { ok: true };
    await sock.readMessages(keys.map((k) => ({ remoteJid: k.chat, id: k.id, participant: k.participant ?? undefined })));
    return { ok: true };
  }
}
