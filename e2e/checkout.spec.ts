import { expect, test, type Page } from "@playwright/test";

const shots = process.env.E2E_SHOTS;
async function shot(page: Page, name: string) {
  if (shots) await page.screenshot({ path: `${shots}/${name}.png`, fullPage: false });
}

async function rpc(page: Page, cmd: string, args: Record<string, unknown> = {}, token: string | null = null) {
  const res = await page.request.post("/rpc", { data: { cmd, token, args } });
  const body = await res.json();
  if (!body.ok) throw new Error(`${cmd}: ${JSON.stringify(body.error)}`);
  return body.data;
}

test("first run → products → offline checkout → refund → shift close", async ({ page }) => {
  await page.goto("/");
  // ---- Setup wizard ----
  await expect(page.getByRole("heading", { name: "Welcome to AMWAPOS" })).toBeVisible();
  await shot(page, "01-welcome");
  await page.getByRole("button", { name: /Continue/ }).click();
  await page.getByLabel("Business name").fill("Al Noor Supermarket");
  await page.getByLabel("VAT number").fill("200000000000003");
  await page.getByLabel("CR number").fill("12345-1");
  await page.getByRole("button", { name: /Continue/ }).click();
  await page.getByRole("button", { name: /Continue/ }).click(); // branch
  await page.getByRole("button", { name: /Continue/ }).click(); // tax 10%
  await page.getByLabel("Owner name").fill("Zana");
  await page.getByLabel("PIN", { exact: true }).fill("4826");
  await page.getByLabel("Confirm PIN").fill("4826");
  await page.getByRole("button", { name: /Continue/ }).click();
  for (let i = 0; i < 4; i++) await page.getByRole("button", { name: /Continue/ }).click(); // terminal, receipt, printer, backup
  await shot(page, "02-review");
  await page.getByRole("button", { name: "Finish Setup" }).click();

  // ---- Login ----
  await expect(page.getByRole("heading", { name: "Who is signing in?" })).toBeVisible();
  await shot(page, "03-login");
  await page.getByRole("button", { name: /Zana/ }).click();
  await page.getByLabel("PIN").fill("4826");
  await page.getByRole("button", { name: "Log in" }).click();

  // ---- Shift open ----
  await expect(page.getByRole("heading", { name: "Start Shift" })).toBeVisible();
  await page.getByLabel("Opening float (cash in drawer)").fill("20.000");
  await shot(page, "04-shift-open");
  await page.getByRole("button", { name: "Open Shift" }).click();
  await expect(page.getByTestId("pos")).toBeVisible();

  // Seed catalogue through the same API the admin screens use.
  const token = await page.evaluate(() => sessionStorage.getItem("amwapos.session"));
  const tax = (await rpc(page, "tax.list", {}, token))[0].tax_rule_id;
  const cat = await rpc(page, "categories.save", { name: "Beverages" }, token);
  const mk = (name: string, barcode: string, price: number, stock: number, fav = true) =>
    rpc(
      page,
      "products.create",
      {
        name,
        tax_rule_id: tax,
        category_id: cat.category_id,
        price_minor: price,
        cost_minor: Math.round(price * 0.6),
        barcodes: [barcode],
        opening_stock_milli: stock,
        is_favorite: fav,
        unit: "pcs",
        track_inventory: true,
        allow_decimal_quantity: false,
        reorder_point_milli: 5000,
      },
      token,
    );
  await mk("Coca-Cola Original 330ml", "06291100001234", 250, 48000);
  await mk("Almarai Fresh Milk 1L", "6281007031126", 850, 12000);
  await mk("Lays Salted 40g", "6281006511339", 300, 3000);
  await page.reload();
  await expect(page.getByTestId("pos")).toBeVisible();

  // ---- Scan (keyboard wedge) ----
  const scan = page.getByTestId("scan-input");
  await scan.focus();
  await page.keyboard.type("06291100001234", { delay: 5 });
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 0.250");
  await page.keyboard.type("06291100001234", { delay: 5 });
  await page.keyboard.press("Enter");
  await page.keyboard.type("6281007031126", { delay: 5 });
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 1.350");
  await expect(page.getByTestId("line-count")).toContainText("2 lines");
  // Search by name
  await scan.fill("lays");
  await expect(page.getByRole("option").first()).toContainText("Lays Salted 40g");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 1.650");
  // Unknown barcode
  await page.keyboard.type("9999999999999", { delay: 5 });
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("unknown-barcode")).toHaveText("9999999999999");
  await shot(page, "05-unknown");
  await page.getByRole("button", { name: "Cancel" }).click();
  await shot(page, "06-pos-cart");

  // ---- Cash payment with change (F6) ----
  await page.keyboard.press("F6");
  await expect(page.getByTestId("amount-due")).toHaveText("BHD 1.650");
  await page.getByTestId("pay-amount").fill("5.000");
  await expect(page.getByTestId("change")).toContainText("BHD 3.350");
  await shot(page, "07-payment");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("receipt-number")).toHaveText(/T01-0000001/);
  await expect(page.getByTestId("success-change")).toHaveText("BHD 3.350");
  await shot(page, "08-success");
  await page.getByTestId("new-sale").click();
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 0.000");

  // ---- Split tender sale ----
  await page.getByTestId("scan-input").focus();
  await page.keyboard.type("6281007031126", { delay: 5 });
  await page.keyboard.press("Enter");
  await page.getByTestId("pay").click();
  await page.getByRole("radio", { name: "Split" }).click();
  await page.getByLabel("Payment 1 amount").fill("0.500");
  await page.getByRole("button", { name: "Add Payment" }).click();
  await expect(page.getByLabel("Payment 2 amount")).toHaveValue("0.350");
  await page.getByTestId("complete-sale").click();
  await expect(page.getByTestId("receipt-number")).toHaveText(/T01-0000002/);
  await page.getByTestId("new-sale").click();

  // ---- Refund one Coca-Cola from the first receipt ----
  await page.getByRole("button", { name: "Refund" }).click();
  await page.getByPlaceholder(/receipt number/i).fill("T01-0000001");
  await page.getByRole("button", { name: "Open Refund" }).click();
  await page.getByLabel("Refund quantity for Coca-Cola Original 330ml").fill("1");
  await shot(page, "09-refund-select");
  await page.getByRole("button", { name: "Review Refund" }).click();
  await expect(page.getByRole("button", { name: /Confirm Refund BHD 0.250/ })).toBeVisible();
  await page.getByRole("button", { name: /Confirm Refund/ }).click();
  await expect(page.getByText("Refund completed")).toBeVisible();
  await page.getByRole("button", { name: "Done" }).click();

  // ---- Shift close (owner sees expected) ----
  await page.getByRole("button", { name: /More/ }).click();
  await page.getByRole("menuitem", { name: "Close shift" }).click();
  // expected: 20.000 float + 1.650 cash − 0.250 refund + 0.500 split cash = 21.900
  await expect(page.getByText("BHD 21.900")).toBeVisible();
  await page.getByLabel("Counted cash in drawer").fill("21.900");
  await shot(page, "10-shift-close");
  await page.getByRole("button", { name: "Close Shift" }).click();
  await expect(page.getByText("Shift closed")).toBeVisible();
  await page.getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("heading", { name: "Start Shift" })).toBeVisible();

  // ---- Admin: dashboard & reports reflect the activity ----
  await page.getByRole("button", { name: "Admin" }).click();
  await expect(page.getByTestId("admin")).toBeVisible();
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();
  await shot(page, "11-dashboard");
  // net sales 1.650 + 0.850 − 0.250 = 2.250
  await expect(page.getByText("BHD 2.250").first()).toBeVisible();
  await page.getByRole("link", { name: "Products" }).click();
  await expect(page.getByText("Almarai Fresh Milk 1L")).toBeVisible();
  await shot(page, "12-products");
  await page.getByRole("link", { name: "Unknown Barcodes" }).click();
  await expect(page.getByText("9999999999999")).toBeVisible();
  await page.getByRole("link", { name: "Reports" }).click();
  await page.getByRole("button", { name: /VAT/ }).click();
  await expect(page.getByRole("heading", { name: "VAT" })).toBeVisible();
  await shot(page, "13-vat");
  await page.getByRole("link", { name: "Audit" }).click();
  await expect(page.getByText("Audit chain verified")).toBeVisible();
  await page.getByRole("link", { name: "Backups" }).click();
  await page.getByRole("button", { name: "Backup Now" }).click();
  await expect(page.getByText("Backup created and verified")).toBeVisible();
  await shot(page, "14-backups");
  await page.getByRole("link", { name: "Diagnostics" }).click();
  await expect(page.getByText("SQLite integrity: OK")).toBeVisible();
  await shot(page, "15-diagnostics");
});
