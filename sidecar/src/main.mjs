// AMWAPOS sidecar entry point.
//   node src/main.mjs --data-dir DIR [--port 0] [--models DIR]
// The bearer token comes from the AMWAPOS_SIDECAR_TOKEN environment variable
// (the app keeps it in Windows Credential Manager). On success one line is
// printed to stdout:  AMWAPOS_SIDECAR_READY {"port":1234,"pid":42,...}
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import pino from "pino";
import { acquireLock } from "./lock.mjs";
import { createServer } from "./http.mjs";
import { Ocr } from "./ocr.mjs";

const VERSION = JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8")).version;
const here = path.dirname(fileURLToPath(import.meta.url));

function args(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    if (argv[i].startsWith("--")) out[argv[i].slice(2)] = argv[i + 1];
  }
  return out;
}

async function main() {
  const a = args(process.argv.slice(2));
  const log = pino({ level: process.env.AMWAPOS_SIDECAR_LOG ?? "info" }, pino.destination(2));
  const token = process.env.AMWAPOS_SIDECAR_TOKEN ?? "";
  if (token.length < 32) {
    log.error("AMWAPOS_SIDECAR_TOKEN is missing or too short");
    process.exit(2);
  }
  delete process.env.AMWAPOS_SIDECAR_TOKEN;
  if (!a["data-dir"]) {
    log.error("--data-dir is required");
    process.exit(2);
  }
  const dataDir = path.resolve(a["data-dir"]);
  const sideDir = path.join(dataDir, "sidecar");
  fs.mkdirSync(sideDir, { recursive: true });
  const lock = acquireLock(path.join(sideDir, "sidecar.lock"));
  if (!lock.ok) {
    log.error({ pid: lock.pid }, "another sidecar is already running for this data folder");
    process.exit(3);
  }
  const modelsDir = path.resolve(a.models ?? path.join(here, "..", "models"));
  const instanceId = crypto.randomUUID();
  const started = Date.now();
  const ocr = new Ocr({ modelsDir, cacheDir: path.join(sideDir, "ocr-cache"), allowedRoots: [dataDir], log });
  // Baileys is loaded lazily so OCR works even if the WhatsApp module fails to load.
  let wa = null;
  let waError = null;
  const whatsapp = async () => {
    if (wa) return wa;
    if (waError) throw waError;
    try {
      const { WhatsApp } = await import("./whatsapp.mjs");
      wa = new WhatsApp({ dataDir: sideDir, allowedRoots: [dataDir], log });
      return wa;
    } catch (e) {
      waError = e;
      throw e;
    }
  };

  const routes = {
    "GET /health": async () => ({ ok: true, pid: process.pid, version: VERSION, uptime_s: Math.round((Date.now() - started) / 1000) }),
    "GET /identity": async () => ({ app: "amwapos-sidecar", version: VERSION, instance_id: instanceId, data_dir: dataDir, pid: process.pid }),
    "GET /whatsapp/status": async () => (await whatsapp()).status(),
    "POST /whatsapp/start": async () => (await whatsapp()).start(),
    "POST /whatsapp/stop": async () => (await whatsapp()).stop(),
    "POST /whatsapp/logout": async () => (await whatsapp()).logout(),
    "GET /whatsapp/messages": async ({ query }) => (await whatsapp()).messages(query),
    "POST /whatsapp/send": async ({ body }) => (await whatsapp()).send(body),
    "POST /whatsapp/mark-read": async ({ body }) => (await whatsapp()).markRead(body),
    "GET /ocr/status": async () => ocr.status(),
    "POST /ocr/recognize": async ({ body }) => ocr.recognize(body),
    "POST /shutdown": async () => {
      setTimeout(() => shutdown(0), 50);
      return { ok: true };
    },
  };
  const { server, listen } = createServer({ token, routes, log });
  const port = await listen(Number(a.port ?? 0) || 0);

  let closing = false;
  async function shutdown(code) {
    if (closing) return;
    closing = true;
    server.close();
    if (wa) await wa.stop().catch(() => {});
    await ocr.close();
    lock.release();
    process.exit(code);
  }
  process.on("SIGINT", () => shutdown(0));
  process.on("SIGTERM", () => shutdown(0));
  // The app owns our lifetime: when its pipe closes, we exit.
  process.stdin.on("end", () => shutdown(0));
  process.stdin.resume();

  const ocrStatus = ocr.status();
  log.info({ port, ocr: ocrStatus.enabled, languages: ocrStatus.languages }, "sidecar listening on 127.0.0.1");
  process.stdout.write(`AMWAPOS_SIDECAR_READY ${JSON.stringify({ port, pid: process.pid, version: VERSION, instance_id: instanceId, ocr: ocrStatus.enabled })}\n`);
}

main().catch((e) => {
  process.stderr.write(`sidecar failed to start: ${e?.stack ?? e}\n`);
  process.exit(1);
});
