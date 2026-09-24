// Minimal JSON HTTP server for the sidecar. Loopback only, bearer token on
// every request, no browser access (requests carrying an Origin header or a
// non-loopback Host header are refused, which blocks DNS-rebinding pages).
import http from "node:http";
import crypto from "node:crypto";

export class HttpError extends Error {
  constructor(status, code, message, details) {
    super(message);
    this.status = status;
    this.code = code;
    this.details = details;
  }
}

const MAX_BODY = 25 * 1024 * 1024;

function tokenMatches(expected, header) {
  if (typeof header !== "string" || !header.startsWith("Bearer ")) return false;
  const a = crypto.createHash("sha256").update(header.slice(7)).digest();
  const b = crypto.createHash("sha256").update(expected).digest();
  return crypto.timingSafeEqual(a, b);
}

function hostAllowed(host, port) {
  return host === `127.0.0.1:${port}` || host === `localhost:${port}`;
}

async function readJson(req) {
  const chunks = [];
  let size = 0;
  for await (const c of req) {
    size += c.length;
    if (size > MAX_BODY) throw new HttpError(413, "too_large", "Request body is too large.");
    chunks.push(c);
  }
  if (!size) return {};
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    throw new HttpError(400, "bad_json", "Request body is not valid JSON.");
  }
}

/** routes: { "GET /path": async ({ query, body }) => value } */
export function createServer({ token, routes, log }) {
  let port = 0;
  const server = http.createServer(async (req, res) => {
    const send = (status, value) => {
      const body = JSON.stringify(value);
      res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" });
      res.end(body);
    };
    try {
      if (req.headers.origin !== undefined || !hostAllowed(req.headers.host, port)) {
        throw new HttpError(403, "forbidden", "Only the AMWAPOS app may call the sidecar.");
      }
      if (!tokenMatches(token, req.headers.authorization)) {
        throw new HttpError(401, "unauthorized", "Missing or wrong sidecar token.");
      }
      const url = new URL(req.url, `http://127.0.0.1:${port}`);
      const handler = routes[`${req.method} ${url.pathname}`];
      if (!handler) throw new HttpError(404, "not_found", "Unknown sidecar endpoint.");
      const body = req.method === "POST" ? await readJson(req) : {};
      const value = await handler({ query: Object.fromEntries(url.searchParams), body });
      send(200, value ?? { ok: true });
    } catch (e) {
      if (e instanceof HttpError) {
        send(e.status, { error: { code: e.code, message: e.message, details: e.details ?? null } });
      } else {
        log.error({ err: String(e?.message ?? e) }, "request failed");
        send(500, { error: { code: "internal", message: String(e?.message ?? e) } });
      }
    }
  });
  return {
    server,
    listen: (wanted) =>
      new Promise((resolve, reject) => {
        server.once("error", reject);
        server.listen(wanted, "127.0.0.1", () => {
          port = server.address().port;
          resolve(port);
        });
      }),
  };
}
