import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  root: "src/renderer",
  plugins: [react()],
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
