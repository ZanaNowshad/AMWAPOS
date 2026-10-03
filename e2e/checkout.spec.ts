import { crc32, deflateSync } from "node:zlib";
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

/** Rarely used admin pages sit under "More tools" in the sidebar. */
async function openMoreTools(page: Page) {
  const more = page.getByTestId("nav-more");
  if ((await more.getAttribute("aria-expanded")) !== "true") await more.click();
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
  await openMoreTools(page);
  await page.getByRole("link", { name: "Audit" }).click();
  await expect(page.getByText("Audit chain verified")).toBeVisible();
  await page.getByRole("link", { name: "Backups" }).click();
  await page.getByRole("button", { name: "Backup Now" }).click();
  await expect(page.getByText("Backup created and verified").first()).toBeVisible();
  await shot(page, "14-backups");
  await openMoreTools(page);
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
  await expect(page.getByPlaceholder("امسح أو ابحث")).toBeVisible();
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

test("AI assistant: live tool steps and thinking, slash commands, shortcuts, and the till assistant", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Zana/ }).click();
  await page.getByLabel("PIN").fill("4826");
  await page.getByRole("button", { name: "Log in" }).click();
  await expect(page.getByRole("heading", { name: "Start Shift" })).toBeVisible();
  const token = await page.evaluate(() => sessionStorage.getItem("amwapos.session"));
  const features = await rpc(page, "settings.get", { key: "features" }, token);
  await rpc(page, "settings.save", { key: "features", value: { ...features, "ai.enabled": true } }, token);
  // The owner lets cashiers use the assistant at the till (read-only for them).
  const roles = await rpc(page, "roles.list", {}, token);
  const cashier = roles.find((r: { role_id: string }) => r.role_id === "role_cashier");
  await rpc(
    page,
    "roles.save",
    { role_id: "role_cashier", name: cashier.name, permissions: [...cashier.permissions, "ai.use"] },
    token,
  );
  await page.getByRole("button", { name: "Logout" }).click();

  // Till (Sara's open shift): the assistant in a drawer, with the cart as context.
  await page.getByRole("button", { name: /Sara/ }).click();
  await page.getByLabel("PIN").fill("7391");
  await page.getByRole("button", { name: "Log in" }).click();
  await expect(page.getByTestId("pos")).toBeVisible();
  await page.getByTestId("till-ai").click();
  const composer = page.getByTestId("ai-composer");
  await composer.fill("what can you do?");
  await composer.press("Enter");
  await expect(page.getByTestId("ai-thinking").first()).toBeVisible();
  await shot(page, "30-till-ai");
  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: /More/ }).click();
  await page.getByRole("menuitem", { name: "Logout" }).click();

  // Owner → Admin → AI page.
  await page.getByRole("button", { name: /Zana/ }).click();
  await page.getByLabel("PIN").fill("4826");
  await page.getByRole("button", { name: "Log in" }).click();
  await page.getByRole("button", { name: "Admin" }).click();
  await page.getByRole("link", { name: "AI Assistant" }).click();
  await composer.fill("low stock");
  await composer.press("Enter");
  await expect(page.getByTestId("ai-tool-step").first()).toContainText("low_stock");
  await expect(page.getByTestId("ai-thinking").first()).toBeVisible();
  // F1: a slash command reads directly, without the model.
  await composer.fill("/kpi");
  await expect(page.getByTestId("ai-slash-palette")).toBeVisible();
  await composer.press("Enter");
  await expect(page.getByTestId("ai-slash-result")).toContainText("/kpi");
  await shot(page, "31-ai-page");
  // F5: "?" opens the shortcut list when not typing.
  await composer.evaluate((el) => (el as HTMLTextAreaElement).blur());
  await page.keyboard.press("?");
  await expect(page.getByRole("heading", { name: "Keyboard shortcuts" })).toBeVisible();
});

// A 96×96 PNG: white with a red square (stands in for a product packshot).
function packshotPng(): Buffer {
  const w = 96;
  const raw = Buffer.alloc((w * 3 + 1) * w, 255);
  for (let y = 24; y < 72; y++) {
    raw[y * (w * 3 + 1)] = 0;
    for (let x = 24; x < 72; x++) {
      const o = y * (w * 3 + 1) + 1 + x * 3;
      raw[o] = 220;
      raw[o + 1] = 30;
      raw[o + 2] = 40;
    }
  }
  for (let y = 0; y < w; y++) raw[y * (w * 3 + 1)] = 0;
  const chunk = (type: string, data: Buffer) => {
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

test("product pictures: upload in the editor, shown in the catalogue and at the till, placeholder otherwise", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Zana/ }).click();
  await page.getByLabel("PIN").fill("4826");
  await page.getByRole("button", { name: "Log in" }).click();
  await expect(page.getByRole("heading", { name: "Start Shift" })).toBeVisible();
  await page.getByRole("button", { name: "Admin" }).click();
  await page.goto("/#/admin/products/new");

  // New product with a picture: preview first, stored on Save.
  await page.getByLabel("Name").first().fill("Picture Test Juice 250ml");
  await page.getByLabel("Selling price").fill("0.400");
  const field = page.getByTestId("product-image-field");
  await expect(field.getByTestId("product-image")).toHaveAttribute("data-state", "placeholder");
  // A wrong type is refused in the browser before anything is sent.
  await field.getByTestId("product-image-input").setInputFiles({
    name: "notes.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("hello"),
  });
  await expect(field).toContainText("PNG, JPEG, WebP or GIF");
  await field.getByTestId("product-image-input").setInputFiles({
    name: "juice.png",
    mimeType: "image/png",
    buffer: packshotPng(),
  });
  await expect(field.getByTestId("product-image")).toHaveAttribute("data-state", "image");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Product created")).toBeVisible();
  // The saved product shows the stored (normalised) picture and where it came from.
  await expect(page.getByTestId("product-image-field").getByTestId("product-image")).toHaveAttribute(
    "data-state",
    "image",
  );
  await expect(page.getByTestId("product-image-field")).toContainText("Uploaded");
  await shot(page, "40-product-picture");

  // Settings → Product images: on by default, but this computer's administrator switch wins.
  await page.evaluate(() => (location.hash = "#/admin/settings?section=images"));
  const status = page.getByTestId("discovery-status");
  await expect(status).toHaveAttribute("data-availability", "disabled_by_administrator");
  await expect(status).toContainText("Automatic pictures are not running");
  await expect(page.getByRole("checkbox", { name: "Find pictures automatically for new products" })).toBeChecked();
  await expect(page.getByRole("checkbox", { name: "Find pictures automatically for new products" })).toBeDisabled();
  await expect(page.getByRole("button", { name: "Find pictures for products without one" })).toBeDisabled();
  await shot(page, "42-image-settings");

  // Catalogue list: the uploaded product has a picture, the others a placeholder.
  await page.goto("/#/admin/products");
  const row = page.getByRole("row", { name: /Picture Test Juice/ });
  await expect(row.getByTestId("product-image")).toHaveAttribute("data-state", "image");
  const milk = page.getByRole("row", { name: /Almarai Fresh Milk/ });
  await expect(milk.getByTestId("product-image")).toHaveAttribute("data-state", "placeholder");

  // Remove → placeholder again. This server runs with the administrator kill
  // switch (AMWAPOS_IMAGE_SEARCH=off, set for every cargo-run process), so the
  // editor says why no automatic search follows and offers none.
  await row.click();
  await page.getByRole("button", { name: "Remove picture" }).click();
  await expect(page.getByTestId("product-image-field").getByTestId("product-image")).toHaveAttribute(
    "data-state",
    "placeholder",
  );
  await expect(page.getByTestId("auto-image-status")).toContainText("disabled on this computer by the administrator");
  await expect(page.getByRole("button", { name: "Find picture automatically" })).toHaveCount(0);
  // Put it back for the till check.
  await page.getByTestId("product-image-input").setInputFiles({
    name: "juice.png",
    mimeType: "image/png",
    buffer: packshotPng(),
  });
  await expect(page.getByTestId("product-image-field").getByTestId("product-image")).toHaveAttribute(
    "data-state",
    "image",
  );

  // Till search results carry the same picture (nothing is added: the next spec closes this shift).
  await page.goto("/");
  await page
    .getByRole("button", { name: /Logout|Log out/ })
    .first()
    .click();
  await page.getByRole("button", { name: /Sara/ }).click();
  await page.getByLabel("PIN").fill("7391");
  await page.getByRole("button", { name: "Log in" }).click();
  await expect(page.getByTestId("pos")).toBeVisible();
  await page.getByTestId("scan-input").fill("picture test");
  const opt = page.getByRole("option").first();
  await expect(opt).toContainText("Picture Test Juice");
  await expect(opt.getByTestId("product-image")).toHaveAttribute("data-state", "image");
  await shot(page, "41-till-picture");
});
