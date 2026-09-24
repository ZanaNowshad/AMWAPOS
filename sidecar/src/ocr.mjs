// Offline OCR with tesseract.js and the language models bundled in ../models.
// OCR is disabled when a model is missing or does not match models.json.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { createWorker } from "tesseract.js";
import { HttpError } from "./http.mjs";

export class Ocr {
  constructor({ modelsDir, cacheDir, allowedRoots, log }) {
    this.modelsDir = modelsDir;
    this.cacheDir = cacheDir;
    this.allowedRoots = allowedRoots.map((r) => fs.realpathSync(r));
    this.log = log;
    this.workers = new Map();
    this.queue = Promise.resolve();
    this.models = this.checkModels();
  }

  /** Verify each bundled model against the manifest (presence and SHA-256). */
  checkModels() {
    const out = {};
    let manifest = {};
    try {
      manifest = JSON.parse(fs.readFileSync(path.join(this.modelsDir, "models.json"), "utf8")).models ?? {};
    } catch {
      /* no manifest: every model is reported unverified */
    }
    for (const lang of ["eng", "ara"]) {
      const file = path.join(this.modelsDir, `${lang}.traineddata.gz`);
      if (!fs.existsSync(file)) {
        out[lang] = { present: false, verified: false };
        continue;
      }
      const sha = crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
      out[lang] = { present: true, verified: manifest[lang]?.sha256 === sha, sha256: sha };
    }
    return out;
  }

  status() {
    const eng = this.models.eng;
    const enabled = !!(eng?.present && eng?.verified);
    return {
      enabled,
      languages: Object.entries(this.models)
        .filter(([, m]) => m.present && m.verified)
        .map(([l]) => l),
      models: this.models,
      reason: enabled ? null : "The English OCR model is missing or damaged. Reinstall AMWAPOS to restore it.",
    };
  }

  resolveInput(p) {
    if (typeof p !== "string" || !p) throw new HttpError(400, "bad_request", "Missing image path.");
    let real;
    try {
      real = fs.realpathSync(p);
    } catch {
      throw new HttpError(404, "not_found", "Image file not found.");
    }
    const inside = this.allowedRoots.some((r) => real === r || real.startsWith(r + path.sep));
    if (!inside) throw new HttpError(403, "forbidden", "The sidecar only reads files inside the AMWAPOS data folder.");
    return real;
  }

  async worker(langs) {
    const key = langs.join("+");
    if (!this.workers.has(key)) {
      fs.mkdirSync(this.cacheDir, { recursive: true });
      const w = await createWorker(langs, 1, {
        langPath: this.modelsDir,
        cachePath: this.cacheDir,
        gzip: true,
        logger: () => {},
        errorHandler: (e) => this.log.warn({ err: String(e) }, "ocr worker error"),
      });
      this.workers.set(key, w);
    }
    return this.workers.get(key);
  }

  recognize({ path: p, langs }) {
    const st = this.status();
    if (!st.enabled) throw new HttpError(409, "ocr_unavailable", st.reason);
    const wanted = (Array.isArray(langs) && langs.length ? langs : ["eng"]).filter((l) => st.languages.includes(l));
    if (!wanted.length) throw new HttpError(409, "ocr_unavailable", "None of the requested OCR languages is installed.");
    const file = this.resolveInput(p);
    // One recognition at a time keeps memory bounded on a till PC.
    const job = this.queue.then(async () => {
      const w = await this.worker(wanted);
      const { data } = await w.recognize(file, {}, { text: true, blocks: true });
      const lines = [];
      for (const b of data.blocks ?? []) {
        for (const para of b.paragraphs ?? []) {
          for (const l of para.lines ?? []) lines.push({ text: l.text.trim(), confidence: Math.round(l.confidence) });
        }
      }
      return { text: data.text, confidence: Math.round(data.confidence), languages: wanted, lines };
    });
    this.queue = job.catch(() => {});
    return job;
  }

  async close() {
    for (const w of this.workers.values()) await w.terminate().catch(() => {});
    this.workers.clear();
  }
}
