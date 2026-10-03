// Installer smoke test, part 2: drives the INSTALLED AMWAPOS window (not a
// dev server) through WebView2's remote-debugging port, which
// scripts/installer-smoke.ps1 opens with
// WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222.
//
//   node scripts/installer-smoke.mjs first      # fresh install: setup → surfaces → a sale
//   node scripts/installer-smoke.mjs relaunch   # existing data is still there
//
// Screenshots go to $SMOKE_SHOTS when set. Exits non-zero on the first failure.
import { crc32, deflateSync } from "node:zlib";
import { mkdirSync } from "node:fs";
import { chromium, expect } from "@playwright/test";

const mode = process.argv[2] ?? "first";
const shots = process.env.SMOKE_SHOTS;
if (shots) mkdirSync(shots, { recursive: true });
const shot = async (page, name) => {
  if (shots) await page.screenshot({ path: `${shots}/${mode}-${name}.png` });
  console.log(`ok: ${name}`);
};

/** A 96×96 white PNG with a red square (a product picture). */
function packshotPng() {
  const w = 96;
  const raw = Buffer.alloc((w * 3 + 1) * w, 255);
  for (let y = 0; y < w; y++) {
    raw[y * (w * 3 + 1)] = 0;
    if (y < 24 || y >= 72) continue;
    for (let x = 24; x < 72; x++) {
      const o = y * (w * 3 + 1) + 1 + x * 3;
      raw[o] = 220;
      raw[o + 1] = 30;
      raw[o + 2] = 40;
    }
  }
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(td) >>> 0);
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(w, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** One backend command through the app's own Tauri IPC. */
async function rpc(page, cmd, args = {}) {
  return page.evaluate(
    async ([cmd, args]) => {
      const token = sessionStorage.getItem("amwapos.session");
      return window.__TAURI_INTERNALS__.invoke("rpc", { cmd, token, args });
    },
    [cmd, args],
  );
}

async function appPage() {
  let browser;
  for (let i = 0; i < 60 && !browser; i++) {
    try {
      browser = await chromium.connectOverCDP("http://127.0.0.1:9222");
    } catch {
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  if (!browser) throw new Error("the installed app's WebView2 never opened its debugging port");
  const ctx = browser.contexts()[0];
  // The window may still be navigating to the app when the port opens: wait
  // until the app has rendered with the desktop IPC bridge, retrying while
  // the document is being replaced.
  let last = "no page";
  for (let i = 0; i < 60; i++) {
    for (const page of ctx.pages()) {
      try {
        const ready = await page.evaluate(
          () =>
            document.readyState !== "loading" &&
            Boolean(window.__TAURI_INTERNALS__) &&
            (document.getElementById("root")?.childElementCount ?? 0) > 0,
        );
        last = page.url();
        if (ready) {
          page.setDefaultTimeout(30_000);
          console.log(`connected: ${last}`);
          return { browser, page };
        }
      } catch (e) {
        last = `${page.url()} (${e?.message ?? e})`;
      }
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error(`not the desktop app after 60 s: ${last}`);
}

async function login(page) {
  await page.getByRole("button", { name: /Zana/ }).click();
  await page.getByLabel("PIN").fill("4826");
  await page.getByRole("button", { name: "Log in" }).click();
}

async function first(page) {
  // Fresh database: the setup wizard (migrations ran on first start).
  await expect(page.getByRole("heading", { name: "Welcome to AMWAPOS" })).toBeVisible({ timeout: 60_000 });
  await shot(page, "01-welcome");
  await page.getByRole("button", { name: /Continue/ }).click();
  await page.getByLabel("Business name").fill("Smoke Test Mart");
  await page.getByLabel("VAT number").fill("200000000000003");
  await page.getByLabel("CR number").fill("12345-1");
  await page.getByRole("button", { name: /Continue/ }).click();
  await page.getByRole("button", { name: /Continue/ }).click();
  await page.getByRole("button", { name: /Continue/ }).click();
  await page.getByLabel("Owner name").fill("Zana");
  await page.getByLabel("PIN", { exact: true }).fill("4826");
  await page.getByLabel("Confirm PIN").fill("4826");
  await page.getByRole("button", { name: /Continue/ }).click();
  for (let i = 0; i < 4; i++) await page.getByRole("button", { name: /Continue/ }).click();
  await page.getByRole("button", { name: "Finish setup" }).click();
  await expect(page.getByRole("heading", { name: "Who is signing in?" })).toBeVisible();
  await login(page);
  await expect(page.getByRole("heading", { name: "Start shift" })).toBeVisible();
  await shot(page, "02-start-shift");

  // Product with a picture (product-image pipeline on Windows).
  const tax = (await rpc(page, "tax.list"))[0].tax_rule_id;
  const p = await rpc(page, "products.create", {
    name: "Smoke Juice 250ml",
    tax_rule_id: tax,
    unit: "pcs",
    price_minor: 1250,
    barcodes: ["6299999000017"],
    opening_stock_milli: 10_000,
    track_inventory: true,
    image_b64: packshotPng().toString("base64"),
  });
  if (!p.image_hash || p.image_source !== "manual") throw new Error("the product picture was not stored");

  // Admin surfaces.
  await page.getByRole("button", { name: "Admin" }).click();
  await page.evaluate(() => (location.hash = "#/admin/products"));
  const row = page.getByRole("row", { name: /Smoke Juice/ });
  await expect(row.getByTestId("product-image")).toHaveAttribute("data-state", "image");
  await shot(page, "03-products");
  await page.evaluate(() => (location.hash = "#/admin/settings?section=images"));
  await expect(page.getByTestId("discovery-status")).toBeVisible();
  await shot(page, "04-product-images-settings");
  const features = await rpc(page, "settings.get", { key: "features" });
  await rpc(page, "settings.save", { key: "features", value: { ...features, "whatsapp.enabled": true } });
  // Reload so the session picks up the module switch; Admin stays open across it.
  await page.reload();
  await expect(page.getByTestId("admin")).toBeVisible();
  await page.evaluate(() => (location.hash = "#/admin/whatsapp"));
  await page.getByRole("tab", { name: "Catalogue" }).click();
  await expect(page.getByTestId("wa-catalog")).toHaveAttribute("data-capability", /disconnected|checking/);
  await shot(page, "05-whatsapp-catalogue");
  await page.getByRole("tab", { name: "Connection" }).click();
  await shot(page, "06-whatsapp-connection");

  // Till and a cash sale.
  await page.getByTestId("back-to-pos").click();
  await expect(page.getByRole("heading", { name: "Start shift" })).toBeVisible();
  await page.getByLabel("Opening float (cash in drawer)").fill("20.000");
  await page.getByRole("button", { name: "Open Shift" }).click();
  await expect(page.getByTestId("pos")).toBeVisible();
  await page.getByTestId("scan-input").focus();
  await page.keyboard.type("6299999000017", { delay: 5 });
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("cart-total")).toHaveText(/1\.250/);
  await expect(page.locator(".cart-line").first().getByTestId("product-image")).toHaveAttribute("data-state", "image");
  await shot(page, "07-till");
  await page.keyboard.press("F6");
  await page.getByTestId("pay-amount").fill("2.000");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("receipt-number")).toBeVisible();
  await shot(page, "08-sale-done");
}

async function relaunch(page) {
  // Existing data: straight to sign-in, the product and the sale are there.
  await expect(page.getByRole("heading", { name: "Who is signing in?" })).toBeVisible({ timeout: 60_000 });
  await login(page);
  await expect(page.getByTestId("pos")).toBeVisible();
  const found = await rpc(page, "products.search", { q: "Smoke Juice" });
  if ((found.rows ?? found).length !== 1) throw new Error("the product did not survive the restart");
  const sales = await rpc(page, "sales.list", {});
  const n = (sales.rows ?? sales).length;
  if (n < 1) throw new Error("the sale did not survive the restart");
  await shot(page, "01-after-restart");
}

let page;
try {
  ({ page } = await appPage());
  if (mode === "first") await first(page);
  else await relaunch(page);
  console.log(`installer smoke (${mode}): passed`);
} catch (e) {
  if (shots && page) await page.screenshot({ path: `${shots}/${mode}-FAILED.png` }).catch(() => {});
  console.error(`installer smoke (${mode}) FAILED: ${e?.message ?? e}`);
  process.exitCode = 1;
}
// Only disconnect: closing a CDP-connected browser could close the app window.
process.exit(process.exitCode ?? 0);
