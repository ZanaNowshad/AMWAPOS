import { expect, test, type Page } from "@playwright/test";

// The primary cashier display: 1024×768 CSS pixels at 100% scaling, touch-first.
// Each state is screenshotted (E2E_SHOTS) and the PAY button is measured: it must be
// at least 200×56, fully on screen with an 8 px bottom margin, never horizontally
// scrolled away, and the topmost element at its centre (nothing covers it).
test.use({ viewport: { width: 1024, height: 768 } });
test.describe.configure({ mode: "serial" });

const shots = process.env.E2E_SHOTS;
async function shot(page: Page, name: string) {
  await page.waitForTimeout(250); // let 150–200 ms transitions settle
  if (shots) await page.screenshot({ path: `${shots}/1024-${name}.png` });
}

async function rpc(page: Page, cmd: string, args: Record<string, unknown> = {}, token: string | null = null) {
  const res = await page.request.post("/rpc", { data: { cmd, token, args } });
  const body = await res.json();
  if (!body.ok) throw new Error(`${cmd}: ${JSON.stringify(body.error)}`);
  return body.data;
}

async function noHorizontalScroll(page: Page) {
  const w = await page.evaluate(() => Math.max(document.documentElement.scrollWidth, document.body.scrollWidth));
  expect(w, "no horizontal scroll").toBeLessThanOrEqual(1024);
}

async function payIsTappable(page: Page, h = 768) {
  const pay = page.getByTestId("pay");
  await expect(pay).toBeVisible();
  const b = (await pay.boundingBox())!;
  expect(b.width, "PAY width").toBeGreaterThanOrEqual(200);
  expect(b.height, "PAY height").toBeGreaterThanOrEqual(56);
  expect(b.x).toBeGreaterThanOrEqual(0);
  expect(b.x + b.width).toBeLessThanOrEqual(1024);
  expect(b.y + b.height, "PAY clear of a 16 px taskbar strip").toBeLessThanOrEqual(h - 16);
  const top = await page.evaluate(
    ([x, y]) => {
      const el = document.elementFromPoint(x, y);
      return !!el?.closest('[data-testid="pay"]');
    },
    [b.x + b.width / 2, b.y + b.height / 2],
  );
  expect(top, "nothing covers PAY").toBe(true);
  // Chrome budget: top bar ≤ 56, dock ≤ 88.
  const bars = await page.evaluate(() => ({
    top: document.querySelector(".pos-header")?.getBoundingClientRect().height ?? 0,
    dock: document.querySelector(".pos-dock")?.getBoundingClientRect().height ?? 0,
  }));
  expect(bars.top).toBeLessThanOrEqual(56);
  expect(bars.dock).toBeLessThanOrEqual(88);
  await noHorizontalScroll(page);
}

/** Every visible button on the page is at least 48×48 (hit area, pseudo-elements included via data). */
async function touchTargets(page: Page, scope = "body") {
  const small = await page.evaluate((sel) => {
    const out: string[] = [];
    for (const el of document.querySelectorAll<HTMLElement>(
      `${sel} button, ${sel} [role=button], ${sel} [role=menuitem]`,
    )) {
      const r = el.getBoundingClientRect();
      if (!r.width || !r.height || getComputedStyle(el).visibility === "hidden") continue;
      // A hit area may be extended by an absolutely positioned ::after with negative insets.
      const after = getComputedStyle(el, "::after");
      const on = after.content !== "none" && after.position === "absolute";
      const ey = on ? Math.max(0, -parseFloat(after.top) || 0) * 2 : 0;
      const ex = on ? Math.max(0, -parseFloat(after.left) || 0) * 2 : 0;
      if (r.width + ex < 47.5 || r.height + ey < 47.5)
        out.push(
          `${el.getAttribute("aria-label") ?? el.textContent?.trim().slice(0, 30)} ${Math.round(r.width)}×${Math.round(r.height)}`,
        );
    }
    return out;
  }, scope);
  expect(small, "touch targets under 48×48").toEqual([]);
}

let barcodes: string[] = [];

async function login(page: Page, name: string, pin: string) {
  await page.getByRole("button", { name: new RegExp(name) }).click();
  await page.getByLabel(/PIN|الرمز السري/).fill(pin);
  await page.getByRole("button", { name: /Log in|تسجيل الدخول/ }).click();
}

async function scan(page: Page, code: string) {
  await page.getByTestId("scan-input").focus();
  await page.keyboard.type(code, { delay: 2 });
  await page.keyboard.press("Enter");
}

test("1024×768: POS, payment, shift close, refund", async ({ page }) => {
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
  const owner = users.find((u: { display_name: string }) => u.display_name === "Zana");
  const token = (await rpc(page, "auth.login", { user_id: owner.user_id, pin: "4826" })).token;
  const tax = (await rpc(page, "tax.list", {}, token))[0].tax_rule_id;
  const cat = await rpc(page, "categories.save", { name: "Grocery 1024" }, token);
  const names = [
    "Basmati Rice 5kg Premium Long Grain",
    "Sunflower Oil 1.8L",
    "Nido Milk Powder 900g",
    "Lipton Yellow Label 100 bags",
    "Kiri Cream Cheese 12 portions",
    "Tang Orange 2kg",
    "Al Kabeer Samosa 20pcs",
    "Galaxy Chocolate 36g",
    "Pril Dishwashing Liquid 1L",
    "Fine Tissues 200 sheets",
    "Masafi Water 1.5L × 6",
    "Americana Chicken Nuggets 400g",
  ];
  barcodes = names.map((_, i) => `77012300${String(i + 10).padStart(4, "0")}`);
  // Re-runs on the same data directory: a duplicate barcode just means the product is there.
  for (let i = 0; i < names.length; i++)
    await rpc(
      page,
      "products.create",
      {
        name: names[i],
        tax_rule_id: tax,
        category_id: cat.category_id,
        price_minor: 350 + i * 1275,
        cost_minor: 200 + i * 700,
        barcodes: [barcodes[i]],
        // Three items sit at the reorder point, so one sale leaves them low.
        opening_stock_milli: i % 4 === 0 ? 6000 : 80000,
        reorder_point_milli: 5000,
        is_favorite: true,
        unit: "pcs",
        track_inventory: true,
      },
      token,
    ).catch(() => undefined);
  // Another cashier's shift left open by an earlier suite blocks this till: close it as the owner.
  const shifts = await rpc(page, "shift.list", { limit: 50 }, token);
  for (const sh of shifts as { shift_id: string; status: string; user_name?: string; expected_cash_minor?: number }[])
    if (sh.status === "open")
      await rpc(
        page,
        "shift.close",
        {
          shift_id: sh.shift_id,
          counted_cash_minor: sh.expected_cash_minor ?? 0,
          note: "closed by the 1024 layout test",
          operation_id: `layout-${sh.shift_id}`,
        },
        token,
      );
  await rpc(page, "auth.logout", {}, token);

  await page.reload();
  await login(page, "Zana", "4826");
  const gate = page.getByRole("heading", { name: "Start shift" });
  await expect(gate.or(page.getByTestId("pos"))).toBeVisible();
  if (await gate.isVisible()) {
    await page.getByLabel("Opening float (cash in drawer)").fill("20.000");
    await page.getByRole("button", { name: "Open Shift" }).click();
  }
  await expect(page.getByTestId("pos")).toBeVisible();

  // ---- 1. Empty cart ----
  await expect(page.getByTestId("cart-total")).toHaveText("BHD 0.000");
  await shot(page, "01-pos-empty");
  await payIsTappable(page);
  await touchTargets(page, ".pos-root");

  // ---- 2. Twelve lines, low stock, a held ticket ----
  await scan(page, barcodes[1]);
  await page.getByRole("button", { name: /^Hold/ }).click();
  await page.getByRole("button", { name: "Hold Sale" }).click();
  await expect(page.getByTestId("held-count")).toHaveText("1");
  for (const code of barcodes) await scan(page, code);
  await expect(page.getByTestId("line-count")).toContainText("12 lines");
  await expect(page.getByTestId("low-stock-hint").first()).toBeVisible();
  await shot(page, "02-pos-12-lines");
  await payIsTappable(page);
  await touchTargets(page, ".pos-root");

  // A maximized window on a 1024×768 panel loses height to the title bar and taskbar:
  // at 700 px PAY and the payment Confirm must still be fully on screen.
  await page.setViewportSize({ width: 1024, height: 700 });
  await payIsTappable(page, 700);
  await page.getByTestId("pay").click();
  const c700 = (await page.getByTestId("complete-sale").boundingBox())!;
  expect(c700.y + c700.height).toBeLessThanOrEqual(700 - 8);
  await shot(page, "03b-payment-at-700");
  await page.keyboard.press("Escape");
  await page.setViewportSize({ width: 1024, height: 768 });

  // ---- 3. Payment sheet: cash with change ----
  await page.getByTestId("pay").click();
  await expect(page.getByTestId("amount-due")).toBeVisible();
  const sheet = page.getByRole("dialog");
  await expect(sheet.getByTestId("tender-cash")).toBeVisible();
  await page.getByTestId("pay-amount").fill("200.000");
  await expect(page.getByTestId("change")).toBeVisible();
  const confirm = page.getByTestId("complete-sale");
  const cb = (await confirm.boundingBox())!;
  expect(cb.height).toBeGreaterThanOrEqual(64);
  expect(cb.y + cb.height).toBeLessThanOrEqual(768 - 8);
  for (const tid of ["tender-cash", "tender-card", "tender-benefitpay", "tender-split"]) {
    const tb = await sheet.getByTestId(tid).boundingBox();
    if (tb) expect(tb.height, tid).toBeGreaterThanOrEqual(72);
  }
  await shot(page, "03-payment-cash-change");
  await touchTargets(page, "[role=dialog]");
  await confirm.click();
  const receipt = (await page.getByTestId("receipt-number").textContent())!.trim();
  await page.getByTestId("new-sale").click();

  // ---- 4. Shift close ----
  await page.getByTestId("pos-more").click();
  await shot(page, "04a-more-sheet");
  await touchTargets(page, "[role=dialog]");
  await page.getByRole("menuitem", { name: "Close shift" }).click();
  await expect(page.getByRole("heading", { name: /Close shift/ })).toBeVisible();
  const counted = page.getByLabel(/Counted cash/);
  await counted.fill("150.000");
  const closeBtn = page.getByTestId("close-shift-confirm");
  const clb = (await closeBtn.boundingBox())!;
  expect(clb.y + clb.height, "Close shift button on screen").toBeLessThanOrEqual(768 - 8);
  await expect(page.getByTestId("expected-cash")).toBeInViewport();
  await shot(page, "04-shift-close");
  await page.keyboard.press("Escape");

  // ---- 5. Refund step 2 ----
  await page.getByRole("button", { name: "Refund", exact: true }).click();
  await page.getByLabel("Receipt number").fill(receipt);
  await page.getByRole("button", { name: "Open Refund" }).click();
  await expect(page.getByTestId("refund-step-2")).toBeVisible();
  const plus = page.getByRole("button", { name: /^Increase/ }).first();
  await plus.click();
  await shot(page, "05-refund-step2");
  await touchTargets(page, "[role=dialog]");
  await page.keyboard.press("Escape");
});

test("1024×768: PAY Send with pay on delivery → Send rail → ticket sheet → delivered and paid", async ({ page }) => {
  await page.goto("/");
  const users = await rpc(page, "auth.users");
  const owner = users.find((u: { display_name: string }) => u.display_name === "Zana");
  const token = (await rpc(page, "auth.login", { user_id: owner.user_id, pin: "4826" })).token;
  // A customer with a saved address (re-runs: the phone is already saved).
  await rpc(
    page,
    "customers.save",
    { customer: { name: "Maryam Send", phone: "33447788", address: "House 1203, Road 45", active: true } },
    token,
  ).catch(() => undefined);
  await rpc(page, "auth.logout", {}, token);
  await page.reload();
  await login(page, "Zana", "4826");
  await expect(page.getByTestId("pos")).toBeVisible();

  // ---- PAY: Send to the customer, paid on delivery (nothing in the drawer now) ----
  await scan(page, barcodes[2]);
  await page.getByTestId("pay").click();
  await expect(page.getByTestId("fulfil-here")).toHaveAttribute("aria-checked", "true");
  await page.getByTestId("fulfil-send").click();
  await expect(page.getByTestId("complete-sale")).toBeDisabled();
  await page.getByTestId("send-customer-search").fill("Maryam Send");
  await page.getByTestId("send-customer-row").first().click();
  await expect(page.getByTestId("send-address")).toHaveValue("House 1203, Road 45");
  await page.getByTestId("send-area").fill("Riffa");
  await page.getByTestId("pay-on-delivery").click();
  const confirm = page.getByTestId("complete-sale");
  // A free-text line is not enough to send: building and block first.
  await expect(confirm).toBeDisabled();
  await expect(page.getByText("Enter the building and block to send to.")).toBeVisible();
  await page.getByTestId("send-building").fill("1203");
  await page.getByTestId("send-block").fill("905");
  await expect(page.getByTestId("send-preview")).toContainText("Bldg 1203, Block 905, House 1203, Road 45");
  await expect(confirm).toBeEnabled();
  await expect(confirm).toContainText("pay on delivery");
  const cb = (await confirm.boundingBox())!;
  expect(cb.y + cb.height).toBeLessThanOrEqual(768 - 8);
  await shot(page, "08-pay-send-pod");
  await touchTargets(page, "[role=dialog]");
  await confirm.click();
  await page.getByTestId("new-sale").click();

  // ---- The Send rail: badge, the ticket unpaid, PAY never covered ----
  await expect(page.getByTestId("send-badge")).toBeVisible();
  await page.getByTestId("send-btn").click();
  const rail = page.getByTestId("send-rail");
  await expect(rail).toBeVisible();
  const row = rail.getByTestId("ticket-row").filter({ hasText: "Maryam Send" }).first();
  await expect(row).toBeVisible();
  await expect(row.getByTestId("pay-chip")).toHaveAttribute("data-state", "unpaid");
  const rb = (await row.boundingBox())!;
  expect(rb.height, "rail row").toBeGreaterThanOrEqual(56);
  await payIsTappable(page);
  await touchTargets(page, "[data-testid=send-rail]");
  await shot(page, "09-send-rail");
  // Rider hand-over opens from the rail (nobody holds cash yet).
  await rail.getByTestId("rider-handover-open").click();
  await expect(page.getByTestId("rider-handover")).toContainText("No rider is holding cash.");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("rider-handover")).toBeHidden();

  // ---- Ticket sheet: Out → Delivered asks "Paid?" → take cash → paid ----
  await row.click();
  const sheet = page.getByTestId("ticket-sheet");
  await expect(sheet).toBeVisible();
  await expect(sheet).toContainText("House 1203, Road 45");
  await expect(sheet.getByTestId("ticket-step-preparing")).toBeVisible();
  await shot(page, "10-ticket-sheet");
  await sheet.getByTestId("ticket-step-dispatched").click();
  await sheet.getByTestId("ticket-step-delivered").click();
  await page.getByTestId("deliver-take-payment").click();
  await page.getByTestId("record-payment-confirm").click();
  await expect(sheet.getByTestId("pay-chip")).toHaveAttribute("data-state", "paid");
  await expect(sheet).toContainText("Delivered");
  await page.keyboard.press("Escape");
  await rail.getByTestId("rail-tab-done").click();
  await expect(rail.getByTestId("ticket-row").filter({ hasText: "Maryam Send" }).first()).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(rail).toBeHidden();
});

test("1024×768: AI page and till drawer", async ({ page }) => {
  await page.goto("/");
  const users = await rpc(page, "auth.users");
  const owner = users.find((u: { display_name: string }) => u.display_name === "Zana");
  const token = (await rpc(page, "auth.login", { user_id: owner.user_id, pin: "4826" })).token;
  const features = await rpc(page, "settings.get", { key: "features" }, token);
  await rpc(
    page,
    "settings.save",
    { key: "features", value: { ...features, "ai.enabled": true, "ai.mutations": true } },
    token,
  );
  await rpc(page, "auth.logout", {}, token);
  await page.reload();
  await login(page, "Zana", "4826");
  await expect(page.getByTestId("pos")).toBeVisible();

  // ---- 7. Till drawer over a six-line cart: PAY stays fully tappable ----
  for (const code of barcodes.slice(0, 6)) await scan(page, code);
  await expect(page.getByTestId("line-count")).toContainText("6 lines");
  await page.getByTestId("till-ai").click();
  await expect(page.getByTestId("ai-composer")).toBeVisible();
  await page.getByTestId("ai-composer").fill("low stock");
  await page.getByTestId("ai-composer").press("Enter");
  await expect(page.getByTestId("ai-tool-step").first()).toBeVisible();
  await shot(page, "07-till-drawer-6-lines");
  await payIsTappable(page);
  await page.keyboard.press("Escape");

  // ---- 6. AI page: empty, streaming + thinking + two tools, proposal with diff ----
  await page.getByTestId("pos-more").click();
  await page.getByRole("menuitem", { name: "Admin" }).click();
  await page.getByRole("link", { name: "AI Assistant" }).click();
  await page.getByTestId("ai-new-chat").click();
  await expect(page.getByTestId("ai-empty")).toBeVisible();
  await shot(page, "06a-ai-empty");
  await noHorizontalScroll(page);
  const composer = page.getByTestId("ai-composer");
  const cbox = (await composer.boundingBox())!;
  expect(cbox.height).toBeGreaterThanOrEqual(56);
  await composer.fill(`set price of ${"Galaxy Chocolate 36g".toLowerCase()} to 0.100`);
  await composer.press("Enter");
  await expect(page.getByTestId("ai-tool-step")).toHaveCount(2);
  await expect(page.getByTestId("ai-thinking").first()).toBeVisible();
  await shot(page, "06b-ai-tools-thinking");
  const card = page.getByTestId("ai-proposal-card").first();
  await expect(card).toBeVisible();
  await expect(card.getByTestId("proposal-diff")).toBeVisible();
  await expect(card.getByTestId("risk-stripe")).toBeVisible();
  const conf = (await card.getByRole("button", { name: "Confirm" }).boundingBox())!;
  expect(conf.height).toBeGreaterThanOrEqual(56);
  await shot(page, "06c-ai-proposal");
  await noHorizontalScroll(page);
  await touchTargets(page, "[data-testid=ai-page]");
  // Leave the price alone.
  await card.getByRole("button", { name: "Reject" }).click();
});

test("1024×768: Arabic RTL, dark compact, backup + update-needed", async ({ page }) => {
  await page.goto("/");
  // ---- 10. Terminal whose hub needs an update, and backups overdue ----
  await page.route("**/rpc", async (route) => {
    const body = route.request().postDataJSON() as { cmd: string };
    if (body.cmd === "backup.health")
      return route.fulfill({
        json: {
          ok: true,
          data: { component: "backup", state: "warning", summary: "Last backup 3 days ago", details: {} },
        },
      });
    if (body.cmd !== "sync.status") return route.fallback();
    await route.fulfill({
      json: {
        ok: true,
        data: {
          mode: "terminal",
          pending: 3,
          last_error: "The hub runs a newer version. Update this till.",
          last_error_kind: "version_mismatch",
        },
      },
    });
  });
  await login(page, "Zana", "4826");
  await expect(page.getByTestId("pos")).toBeVisible();
  await expect(page.getByTestId("sync-pill")).toContainText("Update needed");
  await expect(page.getByTestId("backup-pill")).toBeVisible();
  await shot(page, "10-backup-and-update-needed");
  await payIsTappable(page);
  await page.getByTestId("pos-more").click();
  await page.getByRole("menuitem", { name: "Admin" }).click();
  await expect(page.getByTestId("backup-alert")).toBeVisible();
  const bh = (await page.getByTestId("backup-alert").boundingBox())!;
  expect(bh.height, "backup banner is one line").toBeLessThanOrEqual(72);
  await shot(page, "10b-admin-backup-banner");
  await noHorizontalScroll(page);
  // Admin on the same panel: rail + flyout nav, 48 px chrome, page body scrolls.
  await touchTargets(page, ".topbar");
  await touchTargets(page, ".sidebar");
  await page.getByRole("button", { name: "Toggle navigation" }).click();
  await expect(page.locator(".admin.flyout")).toBeVisible();
  await shot(page, "10c-admin-nav-flyout");
  await page.getByRole("link", { name: "Products" }).click();
  await expect(page.locator(".admin.flyout")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Products", level: 1 })).toBeVisible();
  await noHorizontalScroll(page);
  await shot(page, "10d-admin-products");
  await page.getByTestId("back-to-pos").click();
  await page.unroute("**/rpc");

  // ---- 9. Dark + compact ----
  await page.evaluate(() => {
    document.documentElement.dataset.theme = "dark";
    document.documentElement.dataset.density = "compact";
  });
  for (const code of barcodes.slice(0, 3)) await scan(page, code);
  await shot(page, "09-dark-compact-pos");
  await payIsTappable(page);
  await touchTargets(page, ".pos-root");
  await page.getByTestId("till-ai").click();
  await shot(page, "09b-dark-compact-ai");
  await page.keyboard.press("Escape");
  await page.evaluate(() => {
    document.documentElement.dataset.theme = "light";
    document.documentElement.dataset.density = "compact";
  });

  // ---- 8. Arabic RTL POS + AI ----
  await page.getByTestId("pos-more").click();
  await page.getByRole("menuitem", { name: "العربية" }).click();
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  await expect(page.getByTestId("pos")).toBeVisible();
  // Mirrored in RTL: the checkout column (TOTAL, PAY) sits on the left of the cart.
  const checkoutBox = (await page.locator(".pos-checkout").boundingBox())!;
  const cartBox = (await page.locator(".cart-lines").boundingBox())!;
  expect(checkoutBox.x).toBeLessThan(cartBox.x);
  await shot(page, "08a-ar-pos");
  await payIsTappable(page);
  await page.getByTestId("till-ai").click();
  await expect(page.getByTestId("ai-composer")).toBeVisible();
  await shot(page, "08b-ar-ai-drawer");
  await payIsTappable(page);
  await page.keyboard.press("Escape");
  await page.getByTestId("pos-more").click();
  await page.getByRole("menuitem", { name: "English" }).click();
  await expect(page.locator("html")).toHaveAttribute("dir", "ltr");
});

test("Display size: the setting zooms the app; at 125% and 150% the till still fits", async ({ page }) => {
  await page.goto("/");
  await login(page, "Zana", "4826");
  await expect(page.getByTestId("pos")).toBeVisible();
  // No catalogue grid on the till: scan bar, quick actions and the cart only.
  await expect(page.locator(".tile-grid, .p-tile, .cat-chips")).toHaveCount(0);

  // Settings → Appearance → Display size.
  await page.getByTestId("pos-more").click();
  await page.getByRole("menuitem", { name: "Admin" }).click();
  await page.evaluate(() => (location.hash = "#/admin/settings?section=appearance"));
  const sizes = page.getByTestId("display-size");
  await expect(sizes).toBeVisible();
  await sizes.getByRole("radio", { name: "125%" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-scale", "125");
  await shot(page, "11a-settings-display-size");
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Appearance saved")).toBeVisible();
  // Back to 100% so later runs start from the default.
  await sizes.getByRole("radio", { name: "100%" }).click();
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.locator("html")).toHaveAttribute("data-scale", "100");
  await page.getByTestId("back-to-pos").click();

  // The desktop zooms the WebView, which is the same as a smaller CSS viewport:
  // 1024×768 at 125% = 819×614, at 150% = 683×512.
  for (const [w, h, tag] of [
    [819, 614, "125"],
    [683, 512, "150"],
  ] as const) {
    await page.setViewportSize({ width: w, height: h });
    for (const code of barcodes.slice(0, 5)) await scan(page, code);
    await shot(page, `11-scale-${tag}-pos`);
    const pay = (await page.getByTestId("pay").boundingBox())!;
    expect(pay.width).toBeGreaterThanOrEqual(200);
    expect(pay.height).toBeGreaterThanOrEqual(56);
    // The fold-back bar: nothing covers PAY.
    const onTop = await page.evaluate(
      ([x, y]) => !!document.elementFromPoint(x, y)?.closest('[data-testid="pay"]'),
      [pay.x + pay.width / 2, pay.y + pay.height / 2],
    );
    expect(onTop, `PAY on top at ${tag}%`).toBe(true);
    expect(pay.x + pay.width).toBeLessThanOrEqual(w);
    expect(pay.y + pay.height).toBeLessThanOrEqual(h - 16);
    await expect(page.getByTestId("cart-total")).toBeInViewport();
    await expect(page.getByTestId("scan-input")).toBeInViewport();
    const sw = await page.evaluate(() => document.documentElement.scrollWidth);
    expect(sw, "no horizontal scroll").toBeLessThanOrEqual(w);
    await page.getByTestId("pay").click();
    await page.waitForTimeout(300); // the sheet slides in (170 ms)
    const done = (await page.getByTestId("complete-sale").boundingBox())!;
    expect(done.y + done.height).toBeLessThanOrEqual(h - 8);
    expect(done.x + done.width).toBeLessThanOrEqual(w);
    await shot(page, `11-scale-${tag}-payment`);
    await page.keyboard.press("Escape");
    await page.getByTestId("pos-more").click();
    await page.getByRole("menuitem", { name: "Cancel sale" }).click();
    await expect(page.getByTestId("cart-total")).toHaveText("BHD 0.000");
  }
  await page.setViewportSize({ width: 1024, height: 768 });
});
