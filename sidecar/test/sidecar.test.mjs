import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";

const TOKEN = "t".repeat(48);
const root = path.resolve(import.meta.dirname, "..");
let dir, proc, port;

function start(dataDir) {
  const p = spawn(process.execPath, [path.join(root, "src/main.mjs"), "--data-dir", dataDir], {
    env: { ...process.env, AMWAPOS_SIDECAR_TOKEN: TOKEN, AMWAPOS_SIDECAR_LOG: "error" },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const ready = new Promise((resolve, reject) => {
    let out = "";
    p.stdout.on("data", (d) => {
      out += d;
      const m = out.match(/AMWAPOS_SIDECAR_READY (.*)\n/);
      if (m) resolve(JSON.parse(m[1]));
    });
    p.on("exit", (code) => reject(new Error(`exited ${code}`)));
  });
  return { p, ready };
}

function req(method, pathname, { body, token = TOKEN, headers = {} } = {}) {
  return new Promise((resolve, reject) => {
    const data = body ? JSON.stringify(body) : undefined;
    const r = http.request(
      { host: "127.0.0.1", port, method, path: pathname, headers: { ...(token ? { authorization: `Bearer ${token}` } : {}), ...(data ? { "content-type": "application/json" } : {}), ...headers } },
      (res) => {
        let s = "";
        res.on("data", (d) => (s += d));
        res.on("end", () => resolve({ status: res.statusCode, body: JSON.parse(s) }));
      },
    );
    r.on("error", reject);
    if (data) r.write(data);
    r.end();
  });
}

before(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), "amw-sidecar-"));
  const s = start(dir);
  proc = s.p;
  ({ port } = await s.ready);
});

after(() => {
  proc.kill();
  fs.rmSync(dir, { recursive: true, force: true });
});

test("health and identity need the token", async () => {
  assert.equal((await req("GET", "/health", { token: null })).status, 401);
  assert.equal((await req("GET", "/health", { token: "wrong" })).status, 401);
  const h = await req("GET", "/health");
  assert.equal(h.status, 200);
  assert.equal(h.body.ok, true);
  const id = await req("GET", "/identity");
  assert.equal(id.body.app, "amwapos-sidecar");
});

test("browser-originated and rebinding requests are refused", async () => {
  assert.equal((await req("GET", "/health", { headers: { origin: "http://evil.example" } })).status, 403);
  assert.equal((await req("GET", "/health", { headers: { host: `evil.example:${port}` } })).status, 403);
});

test("a second sidecar on the same data folder refuses to start", async () => {
  const s = start(dir);
  s.ready.catch(() => {});
  const code = await new Promise((resolve) => s.p.on("exit", resolve));
  assert.equal(code, 3);
});

test("whatsapp starts stopped and refuses to send when not ready", async () => {
  const st = await req("GET", "/whatsapp/status");
  assert.equal(st.status, 200);
  assert.equal(st.body.state, "stopped");
  assert.equal(st.body.ready, false);
  const s = await req("POST", "/whatsapp/send", { body: { client_id: "abcdefgh1", to: "97333334444", text: "hi" } });
  assert.equal(s.status, 409);
  assert.equal(s.body.error.code, "not_ready");
});

test("ocr models are verified and English text is read offline", async () => {
  const st = await req("GET", "/ocr/status");
  assert.equal(st.body.enabled, true);
  assert.deepEqual(st.body.languages.sort(), ["ara", "eng"]);
  const img = path.join(dir, "invoice.png");
  fs.copyFileSync(path.join(root, "test/fixtures/invoice.png"), img);
  const r = await req("POST", "/ocr/recognize", { body: { path: img, langs: ["eng"] } });
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.match(r.body.text, /INVOICE 4471/);
  assert.match(r.body.text, /Milk/);
});

test("ocr refuses files outside the data folder", async () => {
  const r = await req("POST", "/ocr/recognize", { body: { path: path.join(root, "test/fixtures/invoice.png") } });
  assert.equal(r.status, 403);
});
