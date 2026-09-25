import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

/**
 * Tauri adds a nonce to style-src for every <style> element in the bundled
 * HTML, and a nonce makes browsers ignore the CSP's 'unsafe-inline', which the
 * styles Sonner inserts at runtime need. Keep the desktop HTML free of them.
 */
function noInlineStyleElements(): Plugin {
  return {
    name: "pap:no-inline-style-elements",
    apply: "build",
    transformIndexHtml: {
      order: "post",
      handler(html) {
        if (/<style[\s>]/i.test(html)) throw new Error("index.html has a <style> element; Tauri would add a style-src nonce and disable 'unsafe-inline'");
      },
    },
  };
}

export default defineConfig(({ mode }) => {
  const web = mode === "web";
  return {
  root: "src/renderer",
  plugins: [react(), tailwindcss(), ...web ? [] : [noInlineStyleElements()]],
  resolve: { alias: {
    "@": fileURLToPath(new URL("./src/renderer", import.meta.url)),
    "#backend": fileURLToPath(new URL(web ? "./src/renderer/backends/http-api.ts" : "./src/renderer/backends/tauri-api.ts", import.meta.url)),
  } },
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
