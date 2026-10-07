import { expect, test, type Page } from "@playwright/test";

// Wave 6 as a person uses it: an offer is created as a draft and switched on
// in Promotions; a coupon code is added; at the till the offer and the coupon
// show their savings, and the sale is paid at the backend's total.

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

async function login(page: Page) {
  await page.goto("/");
  await page.evaluate(() => sessionStorage.clear());
  await page.reload();
  await page.getByRole("button", { name: /Zana/ }).click();
  await page.getByLabel("PIN").fill("4826");
  await page.getByRole("button", { name: "Log in" }).click();
}

async function adminAt(page: Page, hash: string) {
  await login(page);
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

test("an offer and a coupon, from the editor to the till", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  const t = await owner(page);
  const tax = (await rpc(page, "tax.list", {}, t))[0].tax_rule_id;
  const stamp = Date.now() % 100000;
  const prod = await rpc(
    page,
    "products.create",
    {
      name: `Juice W6 ${stamp}`,
      tax_rule_id: tax,
      unit: "pcs",
      track_inventory: true,
      allow_decimal_quantity: false,
      reorder_point_milli: 0,
      price_minor: 1000,
      cost_minor: 600,
      barcodes: [`62999906${String(stamp).padStart(5, "0")}`],
      opening_stock_milli: 50000,
    },
    t,
  );
  const pid = prod.row?.product_id ?? prod.product_id;
  const name = `Juice deal ${stamp}`;
  // A draft offer, created through the same command the editor uses.
  const d = await rpc(
    page,
    "promotions.save",
    {
      promotion: { name, kind: "percent", target: "items", percent_bp: 2000, buy_products: [pid], stackable: true },
      operation_id: `w6-${stamp}-a-0000000000`,
    },
    t,
  );
  const cp = await rpc(
    page,
    "promotions.save",
    {
      promotion: {
        name: `Coupon ${stamp}`,
        kind: "basket",
        target: "items",
        buy_products: [pid],
        threshold_minor: 100,
        percent_bp: 1000,
        requires_coupon: true,
        stackable: true,
      },
      operation_id: `w6-${stamp}-b-0000000000`,
    },
    t,
  );
  await rpc(
    page,
    "promotions.set_status",
    {
      promotion_id: cp.promotion.promotion_id,
      status: "active",
      version: cp.promotion.version,
      operation_id: `w6-${stamp}-c-0000000000`,
    },
    t,
  );
  const code = `SAVE${stamp}`;
  await rpc(
    page,
    "coupons.save",
    { promotion_id: cp.promotion.promotion_id, code, kind: "reusable", operation_id: `w6-${stamp}-d-0000000000` },
    t,
  );

  // Promotions: the draft is there; switch it on from the editor.
  await adminAt(page, "#/admin/promotions");
  await expect(page.getByRole("heading", { name: "Promotions", level: 1 })).toBeVisible();
  const row = page.getByRole("row", { name: new RegExp(name) });
  await expect(row).toContainText("Draft");
  await row.click();
  await expect(page.getByTestId("promo-insight")).toContainText("1 products covered");
  await shot(page, "w6-promotion-editor");
  await page.getByTestId("promo-switch-on").click();
  await expect(page.getByText("Offer updated")).toBeVisible();
  expect((await rpc(page, "promotions.get", { promotion_id: d.promotion.promotion_id }, t)).promotion.status).toBe(
    "active",
  );

  // The till: the offer shows by name, the coupon applies, totals come from the backend.
  await rpc(page, "shift.open", { opening_float_minor: 0, operation_id: `w6-${stamp}-shift-000000000` }, t).catch(
    (e: Error) => {
      if (!String(e.message).includes("already have an open shift")) throw e;
    },
  );
  await login(page);
  await expect(page.getByTestId("pos")).toBeVisible();
  // Start from an empty sale (an earlier flow may have left one open).
  await rpc(page, "pos.cancel", {}, t).catch(() => undefined);
  await rpc(page, "pos.add_product", { product_id: pid, qty_milli: 1000 }, t);
  await page.reload();
  await login(page);
  await expect(page.getByTestId("line-offer")).toContainText(name);
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 0.800");
  await page.getByTestId("coupon-button").click();
  await page.getByTestId("coupon-code").fill(code.toLowerCase());
  await page.getByTestId("coupon-apply").click();
  await expect(page.getByTestId("coupon-state")).toContainText("Applied");
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 0.720");
  await expect(page.getByTestId("savings")).toContainText("0.280");
  await shot(page, "w6-till-offer-coupon");
  // Leave the till clean for other flows.
  await rpc(page, "pos.cancel", {}, t);
});
