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

// The tests share one backend and run in order: the second builds on the store set up by the first.
test.describe.configure({ mode: "serial" });

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
  // No backup yet on a new store: an unmissable banner with one-click Backup Now.
  const alert = page.getByTestId("backup-alert");
  await expect(alert).toContainText("Backup overdue");
  await expect(alert).toContainText("No successful backup yet");
  await shot(page, "11a-backup-alert");
  await alert.getByRole("button", { name: "Backup Now" }).click();
  await expect(page.getByText("Backup created and verified")).toBeVisible();
  await expect(alert).toBeHidden();
  await shot(page, "11-dashboard");
  // net sales 1.650 + 0.850 − 0.250 = 2.250
  await expect(page.getByText("BHD 2.250").first()).toBeVisible();
  await page.getByRole("link", { name: "Products" }).click();
  await expect(page.getByRole("heading", { name: "Products", level: 1 })).toBeVisible();
  await expect(page.getByRole("cell", { name: "Almarai Fresh Milk 1L" })).toHaveCount(1);
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
  await expect(page.getByText("Backup created and verified").first()).toBeVisible();
  await shot(page, "14-backups");
  await page.getByRole("link", { name: "Diagnostics" }).click();
  await expect(page.getByText("SQLite integrity: OK")).toBeVisible();
  await shot(page, "15-diagnostics");
});

test("cashier: no admin access; over-limit discount needs manager approval", async ({ page }) => {
  // Owner creates a cashier through the API.
  const users = await rpc(page, "auth.users");
  const owner = users.find((u: { display_name: string }) => u.display_name === "Zana");
  const ownerToken = (await rpc(page, "auth.login", { user_id: owner.user_id, pin: "4826" })).token;
  const roles = await rpc(page, "roles.list", {}, ownerToken);
  const cashierRole = roles.find((r: { name: string }) => r.name === "Cashier").role_id;
  await rpc(page, "users.create", { user: { display_name: "Sara", role_id: cashierRole, pin: "7391" } }, ownerToken);
  await rpc(page, "auth.logout", {}, ownerToken);

  await page.goto("/");
  await page.getByRole("button", { name: /Sara/ }).click();
  await page.getByLabel("PIN").fill("7391");
  await page.getByRole("button", { name: "Log in" }).click();
  await expect(page.getByRole("heading", { name: "Start Shift" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Admin" })).toHaveCount(0);
  await page.getByLabel("Opening float (cash in drawer)").fill("10.000");
  await page.getByRole("button", { name: "Open Shift" }).click();
  await expect(page.getByTestId("pos")).toBeVisible();

  await page.getByTestId("scan-input").focus();
  await page.keyboard.type("6281007031126", { delay: 5 });
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 0.850");

  // 20% exceeds the cashier's 10% limit → manager approval in place.
  await page.getByRole("listitem").filter({ hasText: "Almarai Fresh Milk 1L" }).click();
  await page.getByRole("button", { name: "Discount", exact: true }).click();
  await page.getByRole("textbox", { name: "Discount", exact: true }).fill("20");
  await page.getByRole("button", { name: "Apply" }).click();
  await expect(page.getByText("Manager Approval")).toBeVisible();
  await shot(page, "16-approval");
  // A wrong PIN is refused and nothing changes.
  await page.getByLabel("Approved by").selectOption(owner.user_id);
  await page.getByLabel("Manager PIN").fill("1111");
  await page.getByRole("button", { name: "Approve" }).click();
  await expect(page.locator(".banner.danger")).toBeVisible();
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 0.850");
  await page.getByLabel("Manager PIN").fill("4826");
  await page.getByRole("button", { name: "Approve" }).click();
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 0.680");

  await page.keyboard.press("F6");
  await page.getByTestId("pay-amount").fill("0.680");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("receipt-number")).toHaveText(/T01-0000003/);
  await page.getByTestId("new-sale").click();

  // Scanner burst: five scans at scanner speed with no waiting in between.
  // None may be dropped and the field must not swallow the next code.
  await page.getByTestId("scan-input").focus();
  for (const code of ["6281007031126", "6281006511339", "6281007031126", "6281006511339", "6281007031126"]) {
    await page.keyboard.type(code, { delay: 2 });
    await page.keyboard.press("Enter");
  }
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 3.150"); // 3 × 0.850 + 2 × 0.300
  // Only consecutive scans of the same item merge (by design), so alternating scans make 5 lines.
  await expect(page.getByTestId("line-count")).toContainText("5 lines · 5 items");
  await expect(page.getByTestId("scan-input")).toHaveValue("");
  await page.keyboard.press("F6");
  await page.getByTestId("pay-amount").fill("3.150");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("receipt-number")).toHaveText(/T01-0000004/);

  // The approval is attributed in the audit trail.
  const t2 = (await rpc(page, "auth.login", { user_id: owner.user_id, pin: "4826" })).token;
  const audit = await rpc(page, "audit.list", { limit: 50 }, t2);
  const rows = audit.rows as { event_type: string; user_name: string | null; approver_name: string | null }[];
  expect(rows.some((r) => r.user_name === "Sara" && r.approver_name === "Zana")).toBe(true);
});

test("Arabic RTL: cashier sale and admin are usable right-to-left", async ({ page }) => {
  await page.goto("/");
  // Switch with the real toggle on the login screen; the choice survives the reload.
  await page.getByTestId("lang-toggle").click();
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  await expect(page.locator("html")).toHaveAttribute("lang", "ar");
  await expect(page.getByRole("heading", { name: "من يسجّل الدخول؟" })).toBeVisible();
  await shot(page, "20-ar-login");

  await page.getByRole("button", { name: /Sara/ }).click();
  await page.getByLabel("الرمز السري").fill("7391");
  await page.getByRole("button", { name: "تسجيل الدخول" }).click();
  await expect(page.getByTestId("pos")).toBeVisible();
  await expect(page.getByPlaceholder("امسح الباركود أو ابحث عن منتج…")).toBeVisible();
  await page.getByTestId("scan-input").focus();
  await page.keyboard.type("6281007031126", { delay: 5 });
  await page.keyboard.press("Enter");
  // Money keeps its left-to-right order inside RTL text.
  await expect(page.getByTestId("cart-total")).toHaveText(/^⁦?BHD 0\.850⁩?$/);
  await shot(page, "21-ar-pos");
  await page.keyboard.press("F6");
  await page.getByTestId("pay-amount").fill("1.000");
  await expect(page.getByTestId("change")).toContainText("BHD 0.150");
  await shot(page, "22-ar-payment");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("receipt-number")).toHaveText(/T01-0000005/);
  await shot(page, "23-ar-success");
  await page.getByTestId("new-sale").click();

  // Owner: admin in Arabic.
  await page.getByRole("button", { name: /المزيد/ }).click();
  await page.getByRole("menuitem", { name: "تسجيل الخروج" }).click();
  await page.getByRole("button", { name: /Zana/ }).click();
  await page.getByLabel("الرمز السري").fill("4826");
  await page.getByRole("button", { name: "تسجيل الدخول" }).click();
  // Shifts are per cashier: the owner lands on the shift gate and goes to Admin from there.
  await expect(page.getByRole("heading", { name: "بدء الوردية" })).toBeVisible();
  await shot(page, "23b-ar-shift-gate");
  await page.getByRole("button", { name: "الإدارة" }).click();
  await expect(page.getByTestId("admin")).toBeVisible();
  await expect(page.getByRole("heading", { name: "لوحة المعلومات" })).toBeVisible();
  await shot(page, "24-ar-dashboard");
  await page.getByRole("link", { name: "المنتجات" }).click();
  await expect(page.getByRole("heading", { name: "المنتجات", level: 1 })).toBeVisible();
  await expect(page.getByRole("cell", { name: "Almarai Fresh Milk 1L" })).toHaveCount(1);
  await shot(page, "25-ar-products");
  await page.getByRole("link", { name: "الإعدادات" }).click();
  await shot(page, "26-ar-settings");
  // Theme and density still apply in RTL.
  await page.evaluate(() => {
    document.documentElement.dataset.theme = "dark";
    document.documentElement.dataset.density = "compact";
  });
  await page.getByRole("link", { name: "لوحة المعلومات" }).click();
  await shot(page, "27-ar-dark-compact");
  await page.evaluate(() => {
    document.documentElement.dataset.theme = "light";
    document.documentElement.dataset.density = "comfortable";
  });
  // Back to English for any later runs on this profile.
  await page.getByTestId("lang-toggle").click();
  await expect(page.locator("html")).toHaveAttribute("dir", "ltr");
});
