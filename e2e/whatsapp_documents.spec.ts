import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";

// Supplier documents and WhatsApp orders against the real backend: the
// bundled OCR reads a synthetic invoice image, a person reviews it and posts a
// receiving draft; a customer chat (fake WhatsApp adapter in the dev bridge)
// becomes a draft order that a person answers and confirms.
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

async function wa(page: Page, data: Record<string, unknown>) {
  const res = await page.request.post("/dev/whatsapp", { data });
  expect(res.ok(), "dev bridge started with --fake-whatsapp").toBe(true);
  return res.json();
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

async function features(page: Page, t: string, on: Record<string, boolean>) {
  const f = await rpc(page, "settings.get", { key: "features" }, t);
  await rpc(page, "settings.save", { key: "features", value: { ...f, ...on } }, t);
}

async function product(page: Page, t: string, name: string, barcode: string, price: number) {
  const tax = (await rpc(page, "tax.list", {}, t))[0].tax_rule_id;
  try {
    await rpc(
      page,
      "products.create",
      {
        name,
        tax_rule_id: tax,
        price_minor: price,
        cost_minor: Math.round(price * 0.6),
        barcodes: [barcode],
        opening_stock_milli: 40_000,
      },
      t,
    );
  } catch (e) {
    if (!String(e).includes("already")) throw e;
  }
}

async function signIn(page: Page) {
  await page.goto("/");
  await page.getByRole("button", { name: /Zana/ }).click();
  await page.getByLabel("PIN").fill("4826");
  await page.getByRole("button", { name: "Log in" }).click();
  // Signed in to the start-of-shift screen, or to the till when an earlier
  // spec left a shift open: both lead to Admin.
  const admin = page.getByRole("button", { name: "Admin" });
  const more = page.getByTestId("pos-more");
  await expect(admin.or(more).first()).toBeVisible();
  if (await admin.isVisible()) await admin.click();
  else {
    await more.click();
    await page.getByRole("menuitem", { name: "Admin" }).click();
  }
}

test("supplier document: OCR → review with evidence → receiving draft → a person receives the stock", async ({
  page,
}) => {
  const t = await owner(page);
  await features(page, t, { "ocr.enabled": true, "ocr.supplier_invoices": true });
  try {
    await rpc(page, "suppliers.save", { supplier: { name: "Gulf Fresh Trading", vat_number: "200099988877766" } }, t);
  } catch (e) {
    if (!String(e).includes("already exists")) throw e;
  }
  await product(page, t, "Milk Full Cream 1L", "6291041500213", 600);
  await product(page, t, "Coca-Cola Original 330 ml", "5449000000996", 150);
  const stockOf = async () => {
    const rows = await rpc(page, "products.search", { q: "Milk Full Cream", limit: 5 }, t);
    return rows.rows.find((r: { name: string }) => r.name === "Milk Full Cream 1L").stock_milli as number;
  };
  const before = await stockOf();

  await signIn(page);
  await page.evaluate(() => (location.hash = "#/admin/invoice-scan"));
  await page.getByRole("button", { name: "Add document" }).click();
  await page.getByLabel("Document file").setInputFiles({
    name: "supplier-invoice.png",
    mimeType: "image/png",
    buffer: readFileSync("e2e/fixtures/supplier-invoice.png"),
  });
  await page.getByRole("button", { name: "Upload and read" }).click();

  // The review page: the bundled OCR runs in the background, then everything is checkable.
  await expect(page.getByText("Ready for review").first()).toBeVisible({ timeout: 90_000 });
  const fields = page.getByTestId("doc-fields");
  await expect(fields.getByLabel("Invoice number")).toHaveValue("GF-2091");
  await expect(page.getByRole("combobox", { name: "Supplier" })).toHaveValue(/.+/);
  await expect(page.getByTestId("doc-validation")).toContainText("Totals add up");
  await expect(page.getByTestId("doc-validation")).toContainText("VAT checks out");
  const lines = page.getByTestId("doc-lines");
  await expect(lines.getByTestId("doc-line-1")).toContainText("Milk Full Cream 1L");
  await expect(lines.getByTestId("doc-line-1")).toContainText("High confidence");
  await expect(lines.getByTestId("doc-line-3")).toContainText("No match");
  // Clicking a line highlights where it was printed on the page.
  await lines.getByTestId("doc-line-1").locator("td").nth(1).click();
  await expect(page.getByTestId("doc-evidence-box")).toBeVisible();
  await shot(page, "doc-review");

  // The unknown product is excluded (never created from an invoice), then a draft is made.
  // (The checkbox follows the saved state, so wait for the save.)
  await lines.getByRole("checkbox", { name: "Include line 3" }).click();
  await expect(lines.getByRole("checkbox", { name: "Include line 3" })).not.toBeChecked();
  // It is still on the invoice: the document checks do not change.
  await expect(page.getByTestId("doc-validation")).toContainText("Totals add up");
  await page.getByTestId("doc-create-receiving").click();
  await expect(page.getByText("Receiving draft created")).toBeVisible();
  expect(await stockOf(), "creating a draft changes no stock").toBe(before);

  // A person with receiving rights posts it through normal receiving.
  await page.getByRole("button", { name: "All documents" }).click();
  await page.getByRole("tab", { name: "Receiving drafts" }).click();
  await page.getByRole("row").filter({ hasText: "Gulf Fresh Trading" }).first().click();
  await page.getByTestId("draft-post").click();
  await page.getByRole("button", { name: "Receive stock" }).click();
  await expect(page.getByText("Stock received")).toBeVisible();
  expect(await stockOf()).toBe(before + 10_000);
});

test("WhatsApp order: chat → draft with a question → staff answer, reply, confirm; nothing automatic", async ({
  page,
}) => {
  const t = await owner(page);
  await product(page, t, "Coca-Cola Original 330 ml", "5449000000996", 150);
  await product(page, t, "Coca-Cola Original 1.5 L", "5449000054227", 600);
  await product(page, t, "Lay's Cheese 50 g", "6281036000011", 250);
  const d = await rpc(page, "settings.get", { key: "delivery" }, t);
  await rpc(
    page,
    "settings.save",
    {
      key: "delivery",
      value: {
        ...d,
        zones: [
          {
            zone_id: "",
            name: "Zone A",
            blocks: [{ from: 200, to: 260 }],
            areas: [],
            fee_minor: 500,
            free_over_minor: null,
            active: true,
          },
        ],
      },
    },
    t,
  );
  await features(page, t, { "whatsapp.enabled": true, "orders.digital": true, "orders.whatsapp_ai": true });
  await expect
    .poll(async () => (await rpc(page, "whatsapp.status", {}, t)).whatsapp.process, { timeout: 20_000 })
    .toBe("stopped");
  await rpc(page, "whatsapp.start", {}, t);
  await wa(page, { action: "scan" });
  await wa(page, {
    action: "deliver",
    messages: [
      {
        wa_id: "e2e-1",
        chat: "97333445566@s.whatsapp.net",
        push_name: "Mariam",
        ts: Math.floor(Date.now() / 1000),
        kind: "text",
        text: "Hi, 2 coke and 1 lays cheese please. Block 230 road 12 building 5 flat 3",
      },
    ],
  });

  await signIn(page);
  await page.evaluate(() => (location.hash = "#/admin/whatsapp-orders"));
  const row = page.getByTestId("wa-order-row").filter({ hasText: "Mariam" });
  await expect(row).toBeVisible({ timeout: 20_000 });
  await row.click();
  const detail = page.getByTestId("wa-order-detail");
  // "coke" fits two sizes: a real question with real options, not a guess.
  await expect(detail.getByTestId("wa-questions")).toContainText("Which Coca-Cola size");
  await expect(detail.getByTestId("wa-delivery")).toContainText("Delivery fee");
  await shot(page, "wa-order-question");
  await detail
    .getByTestId("wa-questions")
    .getByRole("button", { name: /Coca-Cola Original 1\.5 L/ })
    .click();
  await expect(detail.getByTestId("wa-questions")).toHaveCount(0);
  await expect(detail.getByTestId("wa-order-lines")).toContainText("Coca-Cola Original 1.5 L");

  // Staff send a reply only when they press Send.
  expect((await wa(page, { action: "sent" })).sent).toEqual([]);
  await detail
    .getByRole("textbox", { name: "Reply" })
    .fill("Thank you Mariam, your order total is 1.950 BHD with delivery.");
  await detail.getByRole("button", { name: "Send" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Send" }).click();
  await expect(page.getByText("Reply queued on WhatsApp")).toBeVisible();
  await expect.poll(async () => (await wa(page, { action: "sent" })).sent.length, { timeout: 20_000 }).toBe(1);

  // Confirming makes a confirmed digital order: no sale, no payment.
  await detail.getByTestId("wa-confirm").click();
  await page.getByRole("dialog").getByRole("button", { name: "Confirm order" }).click();
  await expect(detail).toContainText("Confirmed");
  const orders = await rpc(page, "orders.list", { status: "confirmed" }, t);
  const o = orders.find((x: { channel: string }) => x.channel === "whatsapp");
  expect(o.payment_state).toBe("unpaid");
  await shot(page, "wa-order-confirmed");

  // The zone that priced the delivery is edited in Settings → Delivery.
  await page.evaluate(() => (location.hash = "#/admin/settings?section=delivery"));
  const zones = page.getByTestId("delivery-zones");
  await expect(zones.getByLabel("Zone name")).toHaveValue("Zone A");
  await expect(zones.getByLabel("Blocks (for example 200-260, 301)")).toHaveValue("200-260");
  await shot(page, "delivery-zones");
});
