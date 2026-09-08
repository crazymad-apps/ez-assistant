import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";
import { version as app_version } from "./package.json" with { type: "json" };

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react(), {
    name: "host-web-version",
    generateBundle() {
      this.emitFile({ type: "asset", fileName: "host-web-manifest.json", source: JSON.stringify({ version: app_version }) });
      const notices = readFileSync(new URL("./licenses/entry-visual.txt", import.meta.url), "utf8")
        .replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
      // 许可证随带内容哈希的发布 assets 内嵌，不扩大 Host 的静态目录白名单。
      this.emitFile({ type: "asset", name: "entry-visual-notices.html", source: `<!doctype html><html lang="en"><meta charset="utf-8"><title>Entry visual notices</title><pre>${notices}</pre></html>` });
    },
  }],
  define: {
    __APP_VERSION__: JSON.stringify(app_version),
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
});
