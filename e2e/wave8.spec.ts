import { expect, test, type Page } from "@playwright/test";

// Wave 8 as a person uses it, against the real backend.
// 1. Document Library: a PDF is added, its text is read per page, search
//    finds the words with the page, the same file again is recognised, and a
//    new version keeps the old one.

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

/** A small PDF with a text layer: one page per entry. */
function pdf(pages: string[][]): Buffer {
  const objs: string[] = [];
  const kids = pages.map((_, i) => `${4 + i * 2} 0 R`).join(" ");
  objs.push("<< /Type /Catalog /Pages 2 0 R >>");
  objs.push(`<< /Type /Pages /Kids [${kids}] /Count ${pages.length} >>`);
  objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
  pages.forEach((lines) => {
    const n = objs.length + 1;
    objs.push(
      `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents ${n + 1} 0 R >>`,
    );
    const body =
      "BT /F1 12 Tf " +
      lines.map((l, j) => `${j === 0 ? "50 780" : "0 -16"} Td (${l.replace(/[()\\]/g, "")}) Tj`).join(" ") +
      " ET";
    objs.push(`<< /Length ${body.length} >>\nstream\n${body}\nendstream`);
  });
  let out = "%PDF-1.4\n";
  const offsets: number[] = [];
  objs.forEach((o, i) => {
    offsets.push(out.length);
    out += `${i + 1} 0 obj\n${o}\nendobj\n`;
  });
  const xref = out.length;
  out += `xref\n0 ${objs.length + 1}\n0000000000 65535 f \n`;
  out += offsets.map((o) => `${String(o).padStart(10, "0")} 00000 n \n`).join("");
  out += `trailer\n<< /Size ${objs.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(out, "latin1");
}

const stamp = Date.now() % 100000;

test("document library: add, read, search with the page, recognise the same file, new version", async ({ page }) => {
  await page.setViewportSize({ width: 1366, height: 900 });
  await owner(page);
  const file = pdf([
    [`LEASE AGREEMENT ${stamp}`, "Between Al Noor Supermarket and the landlord"],
    ["Monthly rent is due on the first day", `Reference R${stamp}X`],
  ]);

  await adminAt(page, "#/admin/documents");
  await expect(page.getByRole("heading", { name: "Document Library" })).toBeVisible();
  await page.getByTestId("library-add").click();
  const form = page.getByTestId("library-add-form");
  await form
    .getByTestId("library-file")
    .setInputFiles({ name: `lease-${stamp}.pdf`, mimeType: "application/pdf", buffer: file });
  await form.getByLabel("Title").fill(`Shop lease ${stamp}`);
  await form.locator("select").selectOption("contract");
  await page.getByRole("button", { name: "Add", exact: true }).click();

  // The drawer: the text was read per page; nothing guessed.
  const drawer = page.getByTestId("library-drawer");
  await expect(drawer).toBeVisible();
  await expect(page.getByRole("heading", { name: new RegExp(`Shop lease ${stamp}`) })).toBeVisible();
  await expect(drawer).toContainText(`LEASE AGREEMENT ${stamp}`);
  await expect(drawer).toContainText("Not entered");
  await shot(page, "w8-01-library-document");
  await drawer.getByRole("button", { name: "Page 2" }).click();
  await expect(drawer).toContainText("Monthly rent is due");
  await page.keyboard.press("Escape");

  // Search finds the words and names the page.
  await page.getByTestId("library-search").fill(`R${stamp}X`);
  await page.getByRole("button", { name: "Search", exact: true }).click();
  const hit = page.getByTestId("library-hit");
  await expect(hit).toHaveCount(1);
  await expect(hit).toContainText("Page 2");
  await shot(page, "w8-02-library-search");

  // The same file again: recognised, not stored twice.
  await page.getByTestId("library-add").click();
  await page
    .getByTestId("library-add-form")
    .getByTestId("library-file")
    .setInputFiles({ name: "copy.pdf", mimeType: "application/pdf", buffer: file });
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await expect(page.getByText(/This exact file is already in the library as DOC-/)).toBeVisible();
  const drawer2 = page.getByTestId("library-drawer");
  await expect(page.getByRole("heading", { name: new RegExp(`Shop lease ${stamp}`) })).toBeVisible();

  // A new version: the old one stays, marked as replaced.
  await drawer2
    .locator('input[type="file"]')
    .setInputFiles({ name: "lease-signed.pdf", mimeType: "application/pdf", buffer: pdf([[`SIGNED LEASE ${stamp}`]]) });
  await expect(page.getByText("Saved as version 2")).toBeVisible();
  await expect(drawer2).toContainText("Version 2");
  await drawer2.getByRole("button", { name: /Version 1/ }).click();
  await expect(drawer2).toContainText("A newer version replaced this one");
  await shot(page, "w8-03-library-versions");
});

test("business memory: a suggestion from a document waits for a person; a written fact is confirmed and can be replaced", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1366, height: 900 });
  const t = await owner(page);
  // A document whose passage becomes a suggestion.
  const doc = await rpc(
    page,
    "library.add",
    {
      document: {
        file_name: `terms-${stamp}.pdf`,
        data_base64: pdf([[`DELIVERY TERMS ${stamp}`, "Deliveries are made on Sundays"]]).toString("base64"),
        category: "contract",
      },
    },
    t,
  );
  await adminAt(page, `#/admin/documents?doc=${doc.document_id}`);
  const drawer = page.getByTestId("library-drawer");
  await expect(drawer).toContainText(`DELIVERY TERMS ${stamp}`);
  await drawer.getByTestId("library-suggest-memory").click();
  const form = page.getByTestId("memory-form");
  await form.getByLabel("The fact, in one sentence").fill(`Deliveries ${stamp} come on Sundays`);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved as a suggestion to review")).toBeVisible();

  // Business Memory: it waits in Needs review until a person confirms it.
  await page.evaluate(() => (location.hash = "#/admin/memory"));
  await expect(page.getByRole("heading", { name: "Business Memory" })).toBeVisible();
  await page.getByRole("tab", { name: /Needs review/ }).click();
  const row = page.getByTestId("memory-row").filter({ hasText: `Deliveries ${stamp}` });
  await expect(row).toHaveCount(1);
  await row.click();
  const md = page.getByTestId("memory-drawer");
  await expect(md).toContainText("From a document");
  await expect(md).toContainText("Deliveries are made on Sundays");
  await shot(page, "w8-04-memory-review");
  await md.getByTestId("memory-confirm").click();
  await expect(md).toContainText("Confirmed");
  await page.keyboard.press("Escape");

  // A person writes a fact down; changing it keeps the old one in history.
  await page.getByTestId("memory-add").click();
  await page
    .getByTestId("memory-form")
    .getByLabel("The fact, in one sentence")
    .fill(`Rent ${stamp} is paid before the 5th`);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  const md2 = page.getByTestId("memory-drawer");
  await expect(md2).toContainText(`Rent ${stamp} is paid before the 5th`);
  await md2.getByRole("button", { name: "Change", exact: true }).click();
  await page
    .getByTestId("memory-form")
    .getByLabel("The fact, in one sentence")
    .fill(`Rent ${stamp} is paid before the 3rd`);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(md2).toContainText(`Rent ${stamp} is paid before the 3rd`);
  await expect(md2).toContainText("Earlier versions");
  await shot(page, "w8-05-memory-history");

  // Secrets are refused.
  await page.keyboard.press("Escape");
  await page.getByTestId("memory-add").click();
  await page.getByTestId("memory-form").getByLabel("The fact, in one sentence").fill("The WiFi password is falcon2026");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByTestId("memory-form")).toContainText("does not keep passwords");
});

test("cash-flow radar: an approved expense is known money going out, linked to its record, never a bank balance", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1366, height: 900 });
  const t = await owner(page);
  const cats = await rpc(page, "expenses.categories", {}, t);
  const ex = await rpc(
    page,
    "expenses.save",
    {
      expense_id: null,
      expense: { category_id: cats[0].category_id, description: `Generator rent ${stamp}`, total_minor: 37_500 },
    },
    t,
  );
  await rpc(page, "expenses.submit", { expense_id: ex.expense_id }, t);
  const st = await rpc(page, "expenses.get", { expense_id: ex.expense_id }, t);
  if (st.expense.status === "submitted")
    await rpc(page, "expenses.decide", { expense_id: ex.expense_id, approve: true }, t);

  await adminAt(page, "#/admin/cashflow");
  await expect(page.getByRole("heading", { name: "Cash-flow Radar" })).toBeVisible();
  await expect(page.getByTestId("radar-not-bank")).toContainText("This is not your bank balance");
  await page.getByRole("tab", { name: "7 days" }).click();
  const line = page.getByTestId("radar-line").filter({ hasText: `Generator rent ${stamp}` });
  await expect(line).toHaveCount(1);
  await expect(line).toContainText("Known");
  await expect(page.getByTestId("radar-known")).not.toContainText("0.000");
  await page.getByTestId("radar-formulas").locator("summary").click();
  await expect(page.getByTestId("radar-formulas")).toContainText("integer minor units");
  await shot(page, "w8-06-cashflow-radar");
  await line.click();
  await expect(page.getByRole("heading", { name: "Expenses" })).toBeVisible();
});
