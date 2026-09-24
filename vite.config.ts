import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The desktop build loads the UI from Tauri; in the browser (development and
// end-to-end tests) `/rpc` is proxied to the loopback dev bridge.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    proxy: { "/rpc": "http://127.0.0.1:8787" },
  },
  build: {
    target: "es2022",
    sourcemap: false,
    chunkSizeWarningLimit: 1500,
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
