import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed dev port and serves the built files from ../ui/dist.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // Network drives (SMB shares) don't deliver file-change events; set LINECARD_POLL=1 there.
    watch: process.env.LINECARD_POLL ? { usePolling: true, interval: 300 } : undefined,
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "es2022",
    outDir: "dist",
    sourcemap: true,
  },
});
