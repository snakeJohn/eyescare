import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 约定：dev 端口 1420，frontendDist 指向 ../dist（见 src-tauri/tauri.conf.json）
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    outDir: "dist",
    target: "chrome105",
    sourcemap: false,
  },
});
