// Publisher tool for signed updates (Ed25519).
//
//   node scripts/sign-update.mjs --gen-key
//       Prints a new private key (PKCS#8 PEM, keep secret) and the public key
//       (base64, 32 bytes) to build into the app as AMWAPOS_UPDATE_PUBKEY.
//
//   UPDATE_SIGNING_KEY="$(cat key.pem)" node scripts/sign-update.mjs \
//       --installer path/AMWAPOS_1.2.0_x64-setup.exe --version 1.2.0 \
//       --url https://updates.example/AMWAPOS_1.2.0_x64-setup.exe [--notes "…"] > latest.json
//
// The app accepts latest.json only if the signature verifies with the built-in
// public key, the version is newer, and the downloaded file matches size+SHA-256.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith("--") ? [...acc, [a.slice(2), all[i + 1]?.startsWith("--") ? true : (all[i + 1] ?? true)]] : acc), []),
);

if (args["gen-key"]) {
  const { privateKey, publicKey } = crypto.generateKeyPairSync("ed25519");
  const raw = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
  process.stdout.write(privateKey.export({ format: "pem", type: "pkcs8" }));
  process.stdout.write(`\nAMWAPOS_UPDATE_PUBKEY=${raw.toString("base64")}\n`);
  process.exit(0);
}

for (const k of ["installer", "version", "url"]) {
  if (!args[k] || args[k] === true) {
    console.error(`missing --${k}`);
    process.exit(2);
  }
}
const pem = process.env.UPDATE_SIGNING_KEY;
if (!pem) {
  console.error("UPDATE_SIGNING_KEY is not set");
  process.exit(2);
}
const bytes = fs.readFileSync(args.installer);
const payload = JSON.stringify({
  version: args.version,
  notes: typeof args.notes === "string" ? args.notes : "",
  published_at: new Date().toISOString(),
  installer: {
    url: args.url,
    sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
    size: bytes.length,
    file_name: path.basename(args.installer),
  },
});
const signature = crypto.sign(null, Buffer.from(payload), crypto.createPrivateKey(pem)).toString("base64");
process.stdout.write(JSON.stringify({ payload, signature }, null, 2) + "\n");
