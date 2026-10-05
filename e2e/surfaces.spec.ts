import fs from "node:fs";
import { expect, test, type Page } from "@playwright/test";

// Every Admin destination, every Settings section and the record pages, as
// the owner with every optional module switched on, in English and Arabic at
// the 1024×768 till panel. Each page must render a heading, show no error
// banner, throw no script error and never scroll sideways. In Arabic the
// interface text (headings, labels, buttons, tabs, table headers, menu) must
// not fall back to English. `E2E_SHOTS=<dir>` saves a screenshot of each.
// The product audit (docs/PRODUCT_UX_FORENSIC_AUDIT.md) cites these runs.

const shots = process.env.E2E_SHOTS;

async function rpc(page: Page, cmd: string, args: Record<string, unknown> = {}, token: string | null = null) {
  const res = await page.request.post("/rpc", { data: { cmd, token, args } });
  const body = await res.json();
  if (!body.ok) throw new Error(`${cmd}: ${JSON.stringify(body.error)}`);
  return body.data;
}

async function owner(page: Page) {
  await page.goto("/");
  const status = await rpc(page, "setup.status");
  if (!status.setup_complete) {
    await rpc(page, "setup.initialize", {
      business_name: "Al Noor Supermarket",
      branch_name: "Main",
      vat_rate_bp: 1000,
      owner_name: "Zana",
      owner_pin: "4826",
      device_name: "Till",
      device_code: "T01",
    });
    await page.reload();
  }
  const users = await rpc(page, "auth.users");
  const o = users.find((u: { display_name: string }) => u.display_name === "Zana");
  return { token: (await rpc(page, "auth.login", { user_id: o.user_id, pin: "4826" })).token as string };
}

const ROUTES = [
  "dashboard",
  "whatsapp-orders",
  "orders",
  "deliveries",
  "payment-reviews",
  "customers",
  "sales",
  "refunds",
  "shifts",
  "cash",
  "products",
  "products/new",
  "categories",
  "pricing",
  "unknown-barcodes",
  "inventory",
  "movements",
  "expiry",
  "waste",
  "stock-cover",
  "stocktake",
  "transfers",
  "suppliers",
  "purchase-orders",
  "receiving",
  "payables",
  "invoice-scan",
  "reports",
  "expenses",
  "analytics",
  "end-of-day",
  "cases",
  "phone-view",
  "whatsapp",
  "ai",
  "users",
  "registers",
  "roles",
  "profile",
  "branches",
  "devices",
  "sync",
  "import",
  "migration",
  "backups",
  "audit",
  "settings",
  "diagnostics",
  "updates",
];

const SETTINGS = [
  "business",
  "tax",
  "pos",
  "shift",
  "payments",
  "receipt",
  "printer",
  "inventory",
  "security",
  "backup",
  "appearance",
  "features",
  "loyalty",
  "delivery",
  "images",
  "whatsapp",
  "ai",
  "about",
];

// Latin words allowed in the Arabic interface: brand, currency, file and
// product names, keys and protocol words people see on their devices.
const LATIN_OK =
  /^(AMWAPOS|BHD|PIN|WhatsApp|OCR|PDF|CSV|ZIP|XLSX|JSON|BenefitPay|IPv4|Ctrl|Esc|Enter|Tab|Shift|OpenAI|Anthropic|Google|OpenRouter|Gemini|Claude|GPT|ESC|POS|EPSON|USB|LAN|Wi|Fi|Hello|Windows|SKU|API|URL|QR|SMS|ID|TCP|IP|HTTP|HTTPS|Tesseract|Bing|Open|Food|Facts|Programmable|Search|Custom|Excel|zip)$/;

interface Finding {
  route: string;
  lang: string;
  kind: string;
  detail: string;
}

async function sweep(page: Page, lang: "en" | "ar", findings: Finding[], records: Record<string, string>) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  const all = [...ROUTES, ...SETTINGS.map((s) => `settings?section=${s}`), ...Object.values(records)];
  for (const route of all) {
    errors.length = 0;
    await page.evaluate((r) => (location.hash = `#/admin/${r}`), route);
    const h1 = page.locator("main h1").first();
    try {
      await expect(h1).toBeAttached({ timeout: 15_000 });
    } catch {
      findings.push({ route, lang, kind: "no-heading", detail: "no page heading (h1)" });
    }
    // Let lists load (skeletons go away) before judging the page.
    try {
      await expect(page.locator("main [aria-busy='true']")).toHaveCount(0, { timeout: 15_000 });
    } catch {
      findings.push({ route, lang, kind: "stuck-loading", detail: "still loading after 15 s" });
    }
    const section = /^settings\?section=(.+)$/.exec(route)?.[1];
    if (section) {
      // A deep link must open the section it names, even with Settings already open.
      const active = await page.locator(".subnav [aria-current=page]").count();
      const href = await page.evaluate(() => location.hash);
      if (!active || !href.endsWith(`section=${section}`))
        findings.push({ route, lang, kind: "wrong-section", detail: href });
    }
    await page.waitForTimeout(150);
    const danger = await page.locator("main .banner.danger").allInnerTexts();
    for (const d of danger) findings.push({ route, lang, kind: "error-banner", detail: d.slice(0, 200) });
    const wide = await page.evaluate(() => {
      const el = document.scrollingElement!;
      const main = document.querySelector("main");
      return Math.max(el.scrollWidth - el.clientWidth, main ? main.scrollWidth - main.clientWidth : 0);
    });
    if (wide > 1) findings.push({ route, lang, kind: "horizontal-scroll", detail: `${wide}px` });
    for (const e of errors) findings.push({ route, lang, kind: "script-error", detail: e.slice(0, 200) });
    if (lang === "ar") {
      const texts = await page
        .locator(
          "main h1, main h2, main h3, main label, main th, main [role=tab], .sb-link, main button:not([dir]), .subnav button, .k-label",
        )
        .evaluateAll((els) =>
          els
            .filter((e) => !e.closest("[dir=auto], [dir=ltr], .money, .num, code, pre, .mono"))
            .map((e) => {
              // Data inside the element (names, amounts, codes) is not interface text.
              const c = e.cloneNode(true) as HTMLElement;
              c.querySelectorAll("[dir=auto], [dir=ltr], .money, .num, code, .mono").forEach((x) => x.remove());
              return (c.textContent ?? "").trim();
            })
            .filter(Boolean),
        );
      for (const tx of texts) {
        const words = tx.match(/[A-Za-z]{3,}/g) ?? [];
        const bad = words.filter((w) => !LATIN_OK.test(w));
        if (bad.length) findings.push({ route, lang, kind: "english-in-arabic", detail: tx.slice(0, 120) });
      }
    }
    if (shots) {
      const name = `${lang}-${route.replace(/[^a-z0-9]+/gi, "-")}`;
      await page.screenshot({ path: `${shots}/surface-${name}.png` });
    }
  }
}

test("every Admin destination renders cleanly in English and Arabic at 1024×768", async ({ page }) => {
  test.setTimeout(600_000);
  await page.setViewportSize({ width: 1024, height: 768 });
  const { token: t } = await owner(page);
  const before = await rpc(page, "settings.get", { key: "features" }, t);
  const everything = Object.fromEntries(Object.keys(before).map((k) => [k, true]));
  // The updater and hub change how this computer runs; leave them as they are.
  delete everything.updates;
  delete everything.hub;
  const records: Record<string, string> = {};
  const findings: Finding[] = [];
  try {
    await rpc(page, "settings.save", { key: "features", value: { ...before, ...everything } }, t);
    const products = await rpc(page, "products.search", { q: "", limit: 1 }, t).catch(() => null);
    const pid = products?.rows?.[0]?.product_id ?? products?.[0]?.product_id;
    if (pid) records.product = `products/${pid}`;
    const custs = await rpc(page, "customers.search", { q: "", limit: 1 }, t).catch(() => []);
    if (custs[0]) records.customer = `customers/${custs[0].customer_id}`;
    const sups = await rpc(page, "suppliers.list", {}, t).catch(() => []);
    if (sups[0]) records.supplier = `suppliers/${sups[0].supplier_id}`;
    records.report = "reports/sales";
    records.profit = "reports/operating_profit";
    records.expenseReport = "reports/expenses";

    // Signed in through the UI, as a person would.
    await page.goto("/");
    await page.getByRole("button", { name: /Zana/ }).click();
    await page.getByLabel("PIN").fill("4826");
    await page.getByRole("button", { name: "Log in" }).click();
    const admin = page.getByRole("button", { name: "Admin" });
    const more = page.getByTestId("pos-more");
    await expect(admin.or(more).first()).toBeVisible();
    if (await admin.isVisible()) await admin.click();
    else {
      await more.click();
      await page.getByRole("menuitem", { name: "Admin" }).click();
    }
    await expect(page.getByTestId("admin")).toBeVisible();
    await sweep(page, "en", findings, records);

    await page.getByTestId("lang-toggle").click();
    await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
    await expect(page.getByTestId("admin")).toBeVisible();
    await sweep(page, "ar", findings, records);
  } finally {
    await page.evaluate(() => localStorage.setItem("amwapos.lang", "en")).catch(() => undefined);
    await rpc(page, "settings.save", { key: "features", value: before }, t);
    if (shots) fs.writeFileSync(`${shots}/surface-findings.json`, JSON.stringify(findings, null, 1));
  }
  expect(findings).toEqual([]);
});

// Each built-in staff role sees only the Admin pages it may use, and every
// page it is shown opens cleanly: no permission error, no stuck loading, no
// error banner. A link the backend would refuse is a broken promise.
test("every role only sees Admin pages that work for it", async ({ page }) => {
  test.setTimeout(600_000);
  await page.setViewportSize({ width: 1024, height: 768 });
  const { token: t } = await owner(page);
  const roles: [string, string, string][] = [
    ["role_manager", "Mona Manager", "5731"],
    ["role_accountant", "Adel Accounts", "5732"],
    ["role_inventory", "Isa Stock", "5733"],
  ];
  const existing = await rpc(page, "auth.users");
  for (const [role_id, display_name, pin] of roles) {
    if (!existing.some((u: { display_name: string }) => u.display_name === display_name))
      await rpc(page, "users.create", { user: { display_name, role_id, pin, active: true } }, t);
  }
  const findings: Finding[] = [];
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  for (const [role, name, pin] of roles) {
    await page.goto("/");
    await page.evaluate(() => sessionStorage.clear());
    await page.reload();
    await page.getByRole("button", { name: new RegExp(name) }).click();
    await page.getByLabel("PIN").fill(pin);
    await page.getByRole("button", { name: "Log in" }).click();
    const admin = page.getByRole("button", { name: "Admin" });
    const more = page.getByTestId("pos-more");
    // Back-office roles without the till land straight in Admin.
    await expect(admin.or(more).or(page.getByTestId("admin")).first()).toBeVisible();
    if (!(await page.getByTestId("admin").isVisible())) {
      if (await admin.isVisible()) await admin.click();
      else {
        await more.click();
        await page.getByRole("menuitem", { name: "Admin" }).click();
      }
    }
    await expect(page.getByTestId("admin")).toBeVisible();
    for (const b of await page.getByTestId("nav-more").all()) {
      if ((await b.getAttribute("aria-expanded")) === "false") await b.click();
    }
    const links = await page
      .locator("a.sb-link")
      .evaluateAll((as) => as.map((a) => (a as HTMLAnchorElement).hash.replace("#/admin/", "")));
    expect(links.length).toBeGreaterThan(0);
    for (const route of links) {
      errors.length = 0;
      await page.evaluate((r) => (location.hash = `#/admin/${r}`), route);
      try {
        await expect(page.locator("main h1").first()).toBeAttached({ timeout: 15_000 });
        await expect(page.locator("main [aria-busy='true']")).toHaveCount(0, { timeout: 15_000 });
      } catch {
        findings.push({ route, lang: role, kind: "not-ready", detail: "no heading or still loading" });
      }
      await page.waitForTimeout(150);
      for (const d of await page.locator("main .banner.danger").allInnerTexts())
        findings.push({ route, lang: role, kind: "error-banner", detail: d.slice(0, 200) });
      for (const e of errors) findings.push({ route, lang: role, kind: "script-error", detail: e.slice(0, 200) });
    }
  }
  if (shots) fs.writeFileSync(`${shots}/role-findings.json`, JSON.stringify(findings, null, 1));
  expect(findings).toEqual([]);
});
