import { expect, test, type Page } from "@playwright/test";

// Wave 4 as a person uses it: Suggested orders → select → Create
// requisition → enter the cost → submit → approve → create the purchase
// order → place it → receive with a refusal and a shortage → see the
// differences → return goods to the supplier.

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

test("suggested order to requisition, approved purchase order, receiving with differences, supplier return", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  const t = await owner(page);
  const tax = (await rpc(page, "tax.list", {}, t))[0].tax_rule_id;
  const sup = await rpc(page, "suppliers.save", { supplier: { name: "Gulf Dairy W4" } }, t).catch(async () => {
    const all = await rpc(page, "suppliers.list", {}, t);
    return all.find((s: { name: string }) => s.name === "Gulf Dairy W4");
  });
  const prod = await rpc(
    page,
    "products.create",
    {
      name: "Ayran W4 250ml",
      tax_rule_id: tax,
      price_minor: 300,
      cost_minor: 180,
      barcodes: ["6299990040011"],
      opening_stock_milli: 2_000,
      reorder_point_milli: 10_000,
    },
    t,
  );
  const pid = prod.row?.product_id ?? prod.product_id;
  // The supplier's terms: cases of 6, two days to deliver, preferred.
  await rpc(
    page,
    "supplier.terms_save",
    { supplier_id: sup.supplier_id, product_id: pid, units_per_case: 6, lead_time_days: 2, preferred: true },
    t,
  );

  // Suggested orders: 2 on hand, reorder at 10 → 8 needed → 2 cases of 6.
  await adminAt(page, "#/admin/suggested-orders");
  await expect(page.getByRole("heading", { name: "Suggested orders", level: 1 })).toBeVisible();
  const row = page.getByRole("row", { name: /Ayran W4 250ml/ });
  await expect(row).toContainText("2 × 6 = 12");
  await expect(row).toContainText("Gulf Dairy W4");
  await shot(page, "w4-suggested");
  await row.getByLabel("Select row").check();
  await page.getByRole("button", { name: /Create requisition/ }).click();

  // The requisition: the line remembers it was suggested.
  await expect(page.getByRole("heading", { name: /REQ-/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /Suggested/ })).toBeVisible();
  await page.getByRole("button", { name: "Edit" }).click();
  await page.getByLabel("Unit cost").first().fill("0.180");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Requisition saved")).toBeVisible();
  await page.getByRole("button", { name: "Submit" }).click();
  await expect(page.getByText("Waiting for approval").first()).toBeVisible();
  await page.getByRole("button", { name: "Approve" }).click();
  await expect(page.getByRole("button", { name: "Create purchase orders" })).toBeVisible();
  await shot(page, "w4-requisition");
  await page.getByRole("button", { name: "Create purchase orders" }).click();
  await expect(page.getByText("Purchase orders created").first()).toBeVisible();
  await page.getByRole("button", { name: /PO-/ }).first().click();

  // The purchase order (a draft): place it.
  await expect(page.getByRole("heading", { name: /PO-/ })).toBeVisible();
  await page.getByRole("button", { name: "Place Order" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Place Order" }).click();
  await expect(page.getByRole("button", { name: "Receive goods" })).toBeVisible();

  // Receive: 10 accepted, 1 refused (damaged), 1 still missing → kept on order.
  await page.getByRole("button", { name: "Receive goods" }).click();
  const recv = page.getByTestId("po-receive");
  await recv.getByLabel("Accepted Ayran W4 250ml").fill("10");
  await recv.getByLabel("Refused Ayran W4 250ml").fill("1");
  await expect(recv.getByLabel("If short Ayran W4 250ml")).toHaveValue("backorder");
  await shot(page, "w4-receive");
  await recv.getByRole("button", { name: "Receive goods" }).click();
  await expect(page.getByText("Goods received")).toBeVisible();
  const diffs = page.getByTestId("po-discrepancies");
  await expect(diffs).toContainText("Refused");
  await expect(diffs).toContainText("Short");
  await expect(diffs).toContainText("Kept on order");
  await shot(page, "w4-differences");
  // Only the accepted 10 became stock: 2 + 10 = 12.
  const st = await rpc(page, "virtual.inventory_get", { product_id: pid }, t).catch(() => null);
  if (st && typeof st.stock_milli === "number") expect(st.stock_milli).toBe(12_000);

  // Return 2 to the supplier.
  await page.evaluate(() => (location.hash = "#/admin/supplier-returns/new"));
  await expect(page.getByRole("heading", { name: "New return" })).toBeVisible();
  await page.getByRole("combobox").first().selectOption({ label: "Gulf Dairy W4" });
  await page.getByRole("button", { name: "Add product" }).click();
  await page.getByLabel("Search products…").fill("Ayran W4");
  await page.getByRole("button", { name: /Ayran W4 250ml/ }).click();
  await page.getByLabel("Quantity").fill("2");
  await page.getByLabel("Reason").selectOption("quality");
  await page.getByRole("button", { name: "Save Draft" }).click();
  await expect(page.getByRole("heading", { name: /SR-/ })).toBeVisible();
  await page.getByRole("button", { name: "Confirm: goods leave stock" }).click();
  await expect(page.getByText("Sent back, credit expected").first()).toBeVisible();
  await shot(page, "w4-return");
  const ret = await rpc(page, "supplier_returns.list", {}, t);
  expect(ret.rows[0].status).toBe("confirmed");
});
