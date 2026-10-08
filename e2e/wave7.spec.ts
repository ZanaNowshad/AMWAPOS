import { expect, test, type Page } from "@playwright/test";

// Wave 7 as a person uses it, against the real backend. This store becomes a
// hub; test tills (real in-process terminals paired through the real pairing
// code, see the dev server's --fake-terminal) send records, heartbeats and
// signed requests through the same code the hub API runs.
// 1. Records refused by the hub open one case in the Alert Centre; the
//    person follows it to Sync problems, tries again, and the system closes
//    the case itself once it no longer sees the problem.
// 2. A record no retry can fix is closed without applying, with a reason.
// 3. Terminals shows only what tills reported; a credential is rotated in
//    stages and a lost till is revoked.

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

async function till(page: Page, action: string, args: Record<string, unknown> = {}) {
  const res = await page.request.post("/dev/terminal", { data: { action, ...args } });
  const body = await res.json();
  if (!body.ok) throw new Error(`terminal ${action}: ${JSON.stringify(body.error)}`);
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

const stamp = Date.now() % 100000;
const barcode = `629${String(stamp).padStart(10, "0")}`;

test("refused records: one case, tried again, closed by the system once recovered", async ({ page }) => {
  await page.setViewportSize({ width: 1366, height: 900 });
  const t = await owner(page);
  await rpc(page, "settings.save", { key: "features", value: { hub: true } }, t);
  await rpc(page, "sync.enable_hub", {}, t);
  const tax = (await rpc(page, "tax.list", {}, t))[0].tax_rule_id;
  await rpc(
    page,
    "products.create",
    {
      name: `Dates W7 ${stamp}`,
      tax_rule_id: tax,
      unit: "pcs",
      track_inventory: true,
      allow_decimal_quantity: false,
      reorder_point_milli: 0,
      price_minor: 1500,
      cost_minor: 900,
      barcodes: [barcode],
    },
    t,
  );
  await till(page, "pair", { token: t, code: "T07", name: "Till 7" });
  const sent = await till(page, "sell_send_parts", { token: t, code: "T07", barcode });
  expect(sent.refused).toBeGreaterThan(0);

  // The Alert Centre: one case for the till and the reason.
  await adminAt(page, "#/admin/cases");
  await expect(page.getByRole("heading", { name: "Alert Centre" })).toBeVisible();
  await page.getByTestId("alert-check-now").click();
  const row = page.getByTestId("alert-centre").getByText(/records from Till 7 could not be saved/);
  await expect(row).toHaveCount(1);
  await shot(page, "w7-01-alert-centre");
  await row.click();
  const drawer = page.getByTestId("case-drawer");
  await expect(drawer.getByTestId("case-system-facts")).toContainText("Still happening");
  await drawer.getByTestId("case-ack").click();
  await expect(drawer).toContainText("Seen");
  await shot(page, "w7-02-case");
  await drawer.getByTestId("case-go-fix").click();

  // Sync problems, in plain words.
  await expect(page.getByRole("heading", { name: "Sync problems" })).toBeVisible();
  const list = page.getByTestId("sync-problems");
  await expect(list.getByText("Waiting for a related record").first()).toBeVisible();
  await expect(page.getByTestId("sync-groups")).toContainText("Till 7");
  await shot(page, "w7-03-sync-problems");
  // Too early: still waiting for the sale itself.
  await list.getByRole("button", { name: "Try again" }).first().click();
  await expect(page.getByText("Still could not be saved. The reason is updated.")).toBeVisible();
  // The till sends the sale; everything shown is tried again and saved.
  await till(page, "send_sales", { code: "T07" });
  await page.getByTestId("sync-retry-many").click();
  await expect(page.getByText(/Saved: [1-9]/)).toBeVisible();
  await expect(list.getByText("Every record was saved")).toBeVisible();

  // The system sees it cleared and closes the case itself.
  await page.evaluate(() => (location.hash = "#/admin/cases"));
  await page.getByTestId("alert-check-now").click();
  await expect(page.getByTestId("alert-centre").getByText(/records from Till 7/)).toHaveCount(0);
  await page.getByRole("tab", { name: "Finished" }).click();
  await page
    .getByTestId("alert-centre")
    .getByText(/records from Till 7 could not be saved/)
    .click();
  await expect(page.getByTestId("case-drawer")).toContainText("Finished by the system");
  await expect(page.getByTestId("case-drawer")).toContainText("Recovered by itself");
  await shot(page, "w7-04-recovered");
});

test("a record no retry can fix is closed without applying, with a reason", async ({ page }) => {
  await page.setViewportSize({ width: 1366, height: 900 });
  const t = await owner(page);
  await till(page, "pair", { token: t, code: "T08", name: "Till 8" });
  const forged = await till(page, "forged_sale", { code: "T07", as: "T08" });
  expect(forged.refused).toBe(1);
  await adminAt(page, "#/admin/sync-reconciliation");
  const list = page.getByTestId("sync-problems");
  await expect(list.getByText("Not allowed from this till")).toBeVisible();
  await expect(list.getByRole("button", { name: "Try again" })).toHaveCount(0);
  await list.getByRole("button", { name: "Close…" }).click();
  const confirm = page.getByTestId("sync-close-confirm");
  await page.getByTestId("sync-close-note").fill("Sent by the wrong till; Till 7 sent it itself");
  await expect(confirm).toBeDisabled();
  await page.getByText("I understand this is a money or stock record").click();
  await confirm.click();
  await expect(list.getByText("Every record was saved")).toBeVisible();
  await page.getByRole("tab", { name: "Finished" }).click();
  await expect(list.getByText("Closed without applying").first()).toBeVisible();
  await shot(page, "w7-05-closed");
});

test("terminal health from what tills reported; rotate a credential; revoke a lost till", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const t = await owner(page);
  await till(page, "pair", { token: t, code: "T09", name: "Till 9" });
  await adminAt(page, "#/admin/terminals");
  const table = page.getByTestId("terminals");
  const t9 = table.getByRole("row").filter({ hasText: "Till 9" });
  // Paired, never reported: unknown, nothing filled in.
  await expect(t9).toContainText("Unknown");
  await expect(t9).toContainText("Has not reported yet");
  await till(page, "heartbeat", { code: "T09" });
  await page.reload();
  await expect(t9).toContainText("Healthy");
  await expect(t9).toContainText("Sync protocol: 2");
  await shot(page, "w7-06-terminals");

  // Staged rotation: the till picks it up at its next exchange, proves it at
  // the one after, and the hub makes it current.
  await page.getByTestId("rotate-T09").click();
  await expect(page.getByTestId("credential-T09")).toContainText("Version 2 waiting to be picked up");
  expect((await till(page, "heartbeat", { code: "T09" })).credential_version).toBe(2);
  await till(page, "heartbeat", { code: "T09" });
  await page.reload();
  await expect(page.getByTestId("credential-T09")).toContainText("Version 2");
  await expect(page.getByTestId("credential-T09")).not.toContainText("waiting");

  // Lost: revoked at once, and never with that credential again.
  const t8 = table.getByRole("row").filter({ hasText: "Till 8" });
  await t8.getByRole("button", { name: "Revoke…" }).click();
  await page.getByLabel("Why").fill("Lost on a delivery run");
  await page.getByText(/Lost or stolen: its credential/).click();
  await page.getByTestId("revoke-confirm").click();
  await expect(t8).toContainText("Revoked");
  await expect(t8).toContainText("Lost or stolen");
  await shot(page, "w7-07-revoked");
});
