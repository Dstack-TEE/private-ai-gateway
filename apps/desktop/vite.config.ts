import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

export default defineConfig(({ mode }) => {
  const web = mode === "web";
  return {
  root: "src/renderer",
  plugins: [react(), tailwindcss()],
  resolve: { alias: {
    "@": fileURLToPath(new URL("./src/renderer", import.meta.url)),
    "#backend": fileURLToPath(new URL(web ? "./src/renderer/backends/http-api.ts" : "./src/renderer/backends/tauri-api.ts", import.meta.url)),
  } },
  define: { "import.meta.env.VITE_TARGET": JSON.stringify(web ? "web" : "desktop") },
  build: {
    assetsInlineLimit: 0,
    emptyOutDir: true,
    outDir: web ? "../../runtime/web-dist" : "../../dist",
  },
  clearScreen: false,
  server: {
    strictPort: true,
  },
  };
});
