import { existsSync } from "node:fs";
import { defineConfig } from "@playwright/test";

// End-to-end tests drive the real UI against the real Rust backend through
// the loopback dev bridge (same Runtime/AppCore as the desktop app).
// Use a pre-installed Chromium when present (sandboxed dev containers); otherwise Playwright's own.
const preinstalled = "/opt/pw-browsers/chromium-1194/chrome-linux/chrome";
const chromium = process.env.PW_CHROMIUM ?? (existsSync(preinstalled) ? preinstalled : undefined);

export default defineConfig({
  testDir: "e2e",
  timeout: 120_000,
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:8799",
    viewport: { width: 1366, height: 768 },
    launchOptions: { executablePath: chromium },
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "rm -rf .amwapos-e2e && cargo run -q -p amwapos-devserver -- --data-dir .amwapos-e2e/data --port 8799 --static dist",
    url: "http://127.0.0.1:8799/",
    timeout: 240_000,
    reuseExistingServer: false,
  },
});
