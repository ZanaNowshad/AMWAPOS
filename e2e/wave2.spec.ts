import { expect, test, type Page } from "@playwright/test";

// Wave 2 of the merchant operating system, as a person uses it: the store
// opens with a checklist, the current totals change nothing, a drawer counted
// short becomes a case to look into, and the trading day is closed once into
// a permanent record.

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
  // No shift open: the start-shift screen has its own way into Admin.
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

test("a short drawer becomes a case; the trading day is closed once", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  const t = await owner(page);
  // Sell 2.000 in cash on a 10.000 float, then count 2.500 short.
  const tax = (await rpc(page, "tax.list", {}, t))[0].tax_rule_id;
  await rpc(
    page,
    "products.create",
    {
      name: "Basmati 2kg",
      tax_rule_id: tax,
      price_minor: 2_000,
      cost_minor: 1_200,
      barcodes: ["6299990020013"],
      opening_stock_milli: 30_000,
    },
    t,
  ).catch(() => undefined);
  if (await rpc(page, "shift.current", {}, t)) {
    const s = await rpc(page, "shift.current", {}, t);
    await rpc(
      page,
      "shift.close",
      { shift_id: s.shift_id, counted_cash_minor: s.expected_cash_minor, operation_id: `W2PRE${Date.now()}` },
      t,
    );
  }
  await rpc(page, "shift.open", { opening_float_minor: 10_000, operation_id: `W2OPEN${Date.now()}` }, t);
  const scan = await rpc(page, "pos.scan", { barcode: "6299990020013", qty_milli: 1000 }, t);
  const total = scan.cart.totals.total_minor;
  await rpc(
    page,
    "pos.finalize",
    {
      cart_id: scan.cart.cart_id,
      operation_id: `W2SALE${Date.now()}`,
      tenders: [{ method: "cash", amount_minor: total }],
      expected_total_minor: total,
    },
    t,
  );
  const s = await rpc(page, "shift.current", {}, t);
  await rpc(
    page,
    "shift.close",
    { shift_id: s.shift_id, counted_cash_minor: s.expected_cash_minor - 2_500, operation_id: `W2CLOSE${Date.now()}` },
    t,
  );

  await adminAt(page, "#/admin/end-of-day");
  await expect(page.getByRole("heading", { name: "End of day", level: 1 })).toBeVisible();
  await expect(page.getByTestId("opening")).toContainText("Opening the store");
  await expect(page.getByTestId("day-drawers")).toContainText("2.500");
  await shot(page, "w2-x");
  const closeCard = page.getByTestId("close-day");
  await expect(closeCard).toContainText("Drawer is BHD 2.500 short");
  const btn = page.getByTestId("close-day-button");
  if (await closeCard.getByText("I have read the items above").isVisible()) {
    await expect(btn).toBeDisabled();
    await closeCard.getByText("I have read the items above").click();
  }
  await shot(page, "w2-close-checks");
  await btn.click();
  const view = page.getByTestId("close-view");
  await expect(view).toContainText("Unchanged since it was closed");
  await shot(page, "w2-z");
  await page.keyboard.press("Escape");
  // The same day cannot be closed twice.
  await expect(page.getByTestId("checks-blocking")).toContainText("already closed");
  await expect(btn).toBeDisabled();

  // The difference waits as a case; a manager resolves it with a note.
  await page.evaluate(() => (location.hash = "#/admin/cases"));
  await expect(page.getByRole("heading", { name: "Cases", level: 1 })).toBeVisible();
  await page.getByRole("cell", { name: "Drawer is BHD 2.500 short" }).click();
  const drawer = page.getByTestId("case-drawer");
  await expect(drawer).toContainText("Expected cash");
  await drawer.getByTestId("case-ack").click();
  await expect(drawer).toContainText("Seen");
  await drawer.getByPlaceholder("What was checked or found").fill("Recounted: two notes were under the tray.");
  await drawer.getByTestId("case-resolve").click();
  await expect(drawer).toContainText("Finished by");
  await shot(page, "w2-case");
});
