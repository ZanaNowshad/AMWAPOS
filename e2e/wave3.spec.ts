import { expect, test, type Page } from "@playwright/test";

// Wave 3 as a person uses it: goods are received with a batch and an expiry
// date, the batch shows on the Expiry page by how soon it expires, part of it
// is written off as waste (stock goes down once), and Days of stock left
// says how long the rest will last.

test.describe.configure({ mode: "serial" });

const shots = process.env.E2E_SHOTS;
async function shot(page: Page, name: string) {
  await page.waitForTimeout(250);
  if (shots) await page.screenshot({ path: `${shots}/${name}.png` });
}

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
  return (await rpc(page, "auth.login", { user_id: o.user_id, pin: "4826" })).token as string;
}

async function adminAt(page: Page, hash: string) {
  await page.goto("/");
  await page.evaluate(() => sessionStorage.clear());
  await page.reload();
  await page.getByRole("button", { name: /Zana/ }).click();
  await page.getByLabel("PIN").fill("4826");
  await page.getByRole("button", { name: "Log in" }).click();
  const startShift = page.getByRole("heading", { name: "Start shift" });
  await expect(page.getByTestId("pos").or(startShift)).toBeVisible();
  if (await startShift.isVisible()) {
    await page.getByRole("button", { name: "Admin" }).click();
  } else {
    await page.getByTestId("pos-more").click();
    await page.getByRole("menuitem", { name: "Admin" }).click();
  }
  await page.evaluate((h) => (location.hash = h), hash);
}

function inDays(n: number) {
  const d = new Date(Date.now() + n * 86_400_000 + 3 * 3_600_000); // Bahrain date
  return d.toISOString().slice(0, 10);
}

test("receive a batch with expiry, see it expiring, write some off", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  const t = await owner(page);
  const tax = (await rpc(page, "tax.list", {}, t))[0].tax_rule_id;
  await rpc(
    page,
    "products.create",
    {
      name: "Fresh Laban 1L",
      tax_rule_id: tax,
      price_minor: 650,
      cost_minor: 400,
      barcodes: ["6299990030012"],
      opening_stock_milli: 2_000,
    },
    t,
  ).catch(() => undefined);

  // Receive 12 with a batch code and an expiry 5 days away.
  await adminAt(page, "#/admin/receiving");
  await expect(page.getByRole("heading", { name: "Receiving", level: 1 })).toBeVisible();
  await page.getByPlaceholder("Scan or search product to add…").fill("6299990030012");
  await page.getByPlaceholder("Scan or search product to add…").press("Enter");
  await page.getByLabel("Quantity").first().fill("12");
  await page.getByLabel("Unit cost").first().fill("0.400");
  await page.getByLabel("Batch code").first().fill("LB-2207");
  await page.getByLabel("Expiry date").first().fill(inDays(5));
  await shot(page, "w3-receive");
  await page.getByRole("button", { name: "Receive goods" }).click();
  await expect(page.getByText("Goods received")).toBeVisible();

  // The Expiry page lists it as urgent, with the old 2 units outside batches.
  await page.evaluate(() => (location.hash = "#/admin/expiry"));
  await expect(page.getByRole("heading", { name: "Expiry", level: 1 })).toBeVisible();
  const row = page.getByRole("row", { name: /Fresh Laban 1L/ });
  await expect(row).toContainText("Expires in 5 days");
  await expect(row).toContainText("12");
  await shot(page, "w3-expiry");
  await row.click();
  const drawer = page.getByTestId("lot-drawer");
  await expect(drawer).toContainText("LB-2207");
  await expect(drawer).toContainText("Estimated sold");
  await drawer.getByRole("button", { name: "Record waste" }).click();
  const dlg = page.getByTestId("waste-dialog");
  await dlg.getByLabel("Quantity").fill("3");
  await dlg.getByRole("button", { name: "Damaged" }).click();
  await page.getByTestId("waste-submit").click();
  await expect(page.getByText("Waste recorded")).toBeVisible();
  await expect(drawer).toContainText("9");
  await shot(page, "w3-lot");
  await page.keyboard.press("Escape");

  // Waste page lists it; stock went down once (2 + 12 − 3 = 11).
  await page.evaluate(() => (location.hash = "#/admin/waste"));
  await expect(page.getByRole("heading", { name: "Waste", level: 1 })).toBeVisible();
  await expect(page.getByRole("row", { name: /Fresh Laban 1L/ })).toContainText("Damaged");
  const pid = (await rpc(page, "pos.search", { q: "6299990030012", limit: 1 }, t))[0].product_id;
  const lots = await rpc(page, "lots.product", { product_id: pid }, t);
  expect(lots.stock_milli).toBe(11_000);
  expect(
    lots.unlotted_milli + lots.lots.reduce((a: number, l: { balance_milli: number }) => a + l.balance_milli, 0),
  ).toBe(11_000);

  // Days of stock left explains why it cannot be counted yet.
  await page.evaluate(() => (location.hash = "#/admin/stock-cover"));
  await expect(page.getByRole("heading", { name: "Days of stock left", level: 1 })).toBeVisible();
  await expect(page.getByRole("row", { name: /Fresh Laban 1L/ })).toContainText("Not enough recent sales to estimate");
  await shot(page, "w3-cover");
});
