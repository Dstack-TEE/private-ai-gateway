import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

export default defineConfig({
  root: "src/renderer",
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src/renderer", import.meta.url)) } },
  build: {
    assetsInlineLimit: 0,
    emptyOutDir: true,
    outDir: "../../dist",
  },
  clearScreen: false,
  server: {
    strictPort: true,
  },
});
