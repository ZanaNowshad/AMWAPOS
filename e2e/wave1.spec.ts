import { expect, test, type Page } from "@playwright/test";

// Wave 1 of the merchant operating system, as a person uses it:
// a sale rung up twice is voided at the till (the original stays, marked
// voided); an expense is entered, approved on entry, paid, and shows in
// operating profit; a customer statement is shown and its PDF made.

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

async function tillSignIn(page: Page, t: string) {
  if (!(await rpc(page, "shift.current", {}, t))) {
    await rpc(page, "shift.open", { opening_float_minor: 20_000, operation_id: `W1OPEN${Date.now()}` }, t);
  }
  await page.goto("/");
  await page.evaluate(() => sessionStorage.clear());
  await page.reload();
  await page.getByRole("button", { name: /Zana/ }).click();
  await page.getByLabel("PIN").fill("4826");
  await page.getByRole("button", { name: "Log in" }).click();
  await expect(page.getByTestId("pos")).toBeVisible();
}

test("void at the till: the sale is cancelled, the original stays", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  const t = await owner(page);
  const tax = (await rpc(page, "tax.list", {}, t))[0].tax_rule_id;
  await rpc(
    page,
    "products.create",
    {
      name: "Halwa 250g",
      tax_rule_id: tax,
      price_minor: 1_250,
      cost_minor: 800,
      barcodes: ["6299990010014"],
      opening_stock_milli: 30_000,
    },
    t,
  ).catch(() => undefined);
  await tillSignIn(page, t);
  await page.getByTestId("scan-input").focus();
  await page.keyboard.type("6299990010014", { delay: 5 });
  await page.keyboard.press("Enter");
  await page.getByTestId("pay").click();
  await page.getByRole("button", { name: "Exact" }).click();
  await page.getByTestId("complete-sale").click();
  const receipt = (await page.getByTestId("receipt-number").innerText()).trim();
  await page.getByTestId("new-sale").click();

  await page.getByTestId("pos-more").click();
  await page.getByRole("menuitem", { name: /recent sales/i }).click();
  await page.getByRole("cell", { name: receipt }).click();
  await page.getByTestId("void-sale").click();
  const dlg = page.getByTestId("void-dialog");
  await expect(dlg).toContainText("BHD 1.250");
  await dlg.getByRole("button", { name: "Rang up twice" }).click();
  await shot(page, "w1-void-dialog");
  await dlg.getByRole("button", { name: "Void sale" }).click();
  await expect(page.getByText("Sale voided")).toBeVisible();
  // The sale is still listed, marked voided.
  await expect(page.getByRole("row", { name: new RegExp(receipt) })).toContainText("Voided");
  await shot(page, "w1-voided");
});

test("an expense is entered, paid and counted in operating profit", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  const t = await owner(page);
  await tillSignIn(page, t);
  await page.getByTestId("pos-more").click();
  await page.getByRole("menuitem", { name: "Admin" }).click();
  await page.evaluate(() => (location.hash = "#/admin/expenses"));
  await expect(page.getByRole("heading", { name: "Expenses", level: 1 })).toBeVisible();
  await page.getByTestId("add-expense").click();
  const ed = page.getByTestId("expense-editor");
  await ed.getByLabel("Category").selectOption({ label: "Electricity" });
  await ed.getByLabel("What for").fill("EWA bill September");
  await ed.getByLabel("Amount paid").fill("42.500");
  await ed.getByLabel("Paid to").fill("EWA");
  await shot(page, "w1-expense-new");
  await page.getByTestId("expense-submit").click();
  const drawer = page.getByTestId("expense-drawer");
  // The owner may approve, so it is approved on entry and waits to be paid.
  await expect(drawer).toContainText("To pay");
  await drawer.getByTestId("expense-pay").click();
  const pay = page.getByTestId("expense-pay-dialog");
  await pay.getByRole("button", { name: "Bank transfer" }).click();
  await pay.getByRole("button", { name: /Record payment of BHD 42.500/ }).click();
  await expect(drawer).toContainText("Paid");
  await shot(page, "w1-expense-paid");
  await page.keyboard.press("Escape");

  await page.evaluate(() => (location.hash = "#/admin/reports/operating_profit"));
  await expect(page.getByRole("heading", { name: "Operating profit", level: 1 })).toBeVisible();
  await expect(page.locator(".k-label", { hasText: "Operating expenses" }).locator("..")).toContainText("42.500");
  await shot(page, "w1-operating-profit");
});

test("a customer statement shows the ledger, ageing and a PDF", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  const t = await owner(page);
  await rpc(page, "settings.save", { key: "features", value: { customer_credit: true } }, t);
  const cu = await rpc(page, "customers.save", { customer: { name: "Maryam Statement", phone: "36000077" } }, t);
  await rpc(
    page,
    "customers.account_set",
    { customer_id: cu.customer_id, enabled: true, credit_limit_minor: 100_000 },
    t,
  );
  await rpc(
    page,
    "customers.account_adjust",
    {
      customer_id: cu.customer_id,
      amount_minor: 7_250,
      note: "Opening balance from the old book",
      operation_id: `W1ADJ${Date.now()}`,
    },
    t,
  );
  await tillSignIn(page, t);
  await page.getByTestId("pos-more").click();
  await page.getByRole("menuitem", { name: "Admin" }).click();
  await page.evaluate((id) => (location.hash = `#/admin/customers/${id}`), cu.customer_id);
  await page.getByRole("tab", { name: "Account" }).click();
  const st = page.getByTestId("statement");
  await expect(st).toContainText("Adjustment");
  await expect(st).toContainText("7.250");
  await shot(page, "w1-statement");
  const download = page.waitForEvent("download");
  await st.getByRole("button", { name: "PDF" }).click();
  expect((await download).suggestedFilename()).toMatch(/\.pdf$/);
});
