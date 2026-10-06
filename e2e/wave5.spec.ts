import { expect, test, type Page } from "@playwright/test";

// Wave 5 as a person uses it: set up a scale barcode rule and try a label,
// merge two duplicate products, apply a recommended price from the pricing
// review, then sell a scale label at the till on a phone order.

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

/** A 12-digit body with its EAN check digit. */
function ean(body: string): string {
  let sum = 0;
  body
    .split("")
    .reverse()
    .forEach((d, i) => (sum += Number(d) * (i % 2 === 0 ? 3 : 1)));
  return body + String((10 - (sum % 10)) % 10);
}

test("scale rule, duplicate merge, pricing review and a scale label on a phone order", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  const t = await owner(page);
  const tax = (await rpc(page, "tax.list", {}, t))[0].tax_rule_id;
  const mk = (a: Record<string, unknown>) =>
    rpc(
      page,
      "products.create",
      {
        tax_rule_id: tax,
        unit: "pcs",
        track_inventory: true,
        allow_decimal_quantity: false,
        reorder_point_milli: 0,
        ...a,
      },
      t,
    );
  await mk({
    name: "Halloumi W5",
    unit: "kg",
    allow_decimal_quantity: true,
    price_minor: 3200,
    cost_minor: 1500,
    barcodes: [],
    opening_stock_milli: 20000,
    plu: "4242",
  });
  await mk({
    name: "Laban W5 1L",
    price_minor: 300,
    cost_minor: 250,
    barcodes: ["6299990050011"],
    opening_stock_milli: 6000,
  });
  await mk({
    name: "LABAN W5 1 L",
    price_minor: 300,
    cost_minor: 250,
    barcodes: ["6299990050028"],
    opening_stock_milli: 4000,
  });

  // Scale barcodes: none yet; add a weight rule and try a label.
  await adminAt(page, "#/admin/scale-barcodes");
  await expect(page.getByRole("heading", { name: "Scale barcodes", level: 1 })).toBeVisible();
  await expect(page.getByText("No scale barcode rules configured.")).toBeVisible();
  await page.getByRole("button", { name: "Add rule" }).click();
  await page.getByLabel("Name").fill("Deli weight");
  await page.getByLabel("Starts with").fill("21");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Rule saved")).toBeVisible();
  await expect(page.getByRole("row", { name: /Deli weight/ })).toBeVisible();
  const label = ean("210424201250"); // PLU 4242, 1.250 kg
  await page.getByLabel("Try a code").fill(label);
  await page.getByRole("button", { name: "Check", exact: true }).click();
  await expect(page.getByTestId("scale-test")).toContainText("Halloumi W5");
  await shot(page, "w5-scale-rule");

  // Likely duplicates: the two Laban entries, with evidence; merge them.
  await page.evaluate(() => (location.hash = "#/admin/duplicates"));
  const pair = page.getByTestId("dup-pair").filter({ hasText: "Laban W5" });
  await expect(pair).toBeVisible();
  await expect(pair).toContainText("Same name and size");
  await shot(page, "w5-duplicates");
  await pair.getByRole("button", { name: "Merge…" }).click();
  const dlg = page.getByTestId("merge-dialog");
  await expect(dlg.getByTestId("merge-preview")).toContainText("What moves to the kept product");
  await shot(page, "w5-merge-preview");
  await dlg.getByRole("button", { name: "Merge", exact: true }).click();
  await expect(page.getByText("Merge cannot be undone")).toBeVisible();
  await page.getByRole("dialog", { name: "Merge cannot be undone" }).getByRole("button", { name: "Merge" }).click();
  await expect(page.getByText("Products merged")).toBeVisible();
  await expect(page.getByText("No likely duplicate products found.")).toBeVisible();
  // Stock was conserved: 6 + 4 on the kept product.
  const kept = await rpc(page, "products.search", { q: "Laban W5" }, t);
  const total = kept.rows.reduce((s: number, r: { stock_milli: number }) => s + r.stock_milli, 0);
  expect(total).toBe(10000);

  // Pricing review: a policy finds Laban below its minimum margin.
  await rpc(
    page,
    "pricing.policy_save",
    {
      policy: {
        name: "Store policy",
        scope: "global",
        target_margin_bp: 4000,
        min_margin_bp: 3000,
        rounding_step_minor: 50,
      },
    },
    t,
  );
  await page.evaluate(() => (location.hash = "#/admin/pricing-review?group=below_min_margin"));
  await expect(page.getByRole("heading", { name: "Pricing review", level: 1 })).toBeVisible();
  const row = page.getByRole("row", { name: /Laban W5/ });
  await expect(row).toBeVisible();
  await shot(page, "w5-pricing-review");
  await row.getByRole("button", { name: "Accept" }).click();
  const apply = page.getByTestId("pricing-apply");
  await expect(apply).toContainText("Laban W5");
  await apply.getByRole("button", { name: /Apply 1 price/ }).click();
  await expect(page.getByText("1 price(s) applied")).toBeVisible();
  // Laban now meets its minimum margin and leaves the group.
  await expect(page.getByRole("row", { name: /Laban W5/ })).toHaveCount(0);

  // The till: a phone order with a scale label.
  // The till needs an open shift (an earlier flow may already have one).
  await rpc(page, "shift.open", { opening_float_minor: 0, operation_id: `w5-${Date.now()}` }, t).catch((e: Error) => {
    if (!String(e.message).includes("already have an open shift")) throw e;
  });
  await login(page);
  await expect(page.getByTestId("pos")).toBeVisible();
  await page.getByTestId("sale-channel").selectOption("phone");
  await page.getByTestId("scan-input").focus();
  await page.keyboard.type(label, { delay: 5 });
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("scale-line")).toContainText("Scale label: weight");
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 4.000");
  await expect(page.getByTestId("sale-channel")).toHaveValue("phone");
  await shot(page, "w5-till-scale");
  const cart = await rpc(page, "pos.cart", {}, t);
  expect(cart.channel).toBe("phone");
  expect(cart.lines[0].scale.kind).toBe("weight");
});
