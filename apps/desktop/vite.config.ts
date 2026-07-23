import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";

/** Shared with tauri.conf.json `build.devUrl` query — browser tabs without it get 403. */
const DEV_GATE_TOKEN = "hg-tauri-only";
const DEV_GATE_COOKIE = "hg_tauri_dev";

function tauriDevGate(): Plugin {
  return {
    name: "tauri-dev-gate",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        // Only enforce during Vite dev (tauri embeds this server).
        const url = new URL(req.url || "/", "http://127.0.0.1");
        const cookies = req.headers.cookie || "";
        const hasCookie = cookies
          .split(";")
          .some((c) => c.trim() === `${DEV_GATE_COOKIE}=${DEV_GATE_TOKEN}`);
        const hasQuery = url.searchParams.get("tauri") === DEV_GATE_TOKEN;

        if (hasQuery) {
          // Persist for HMR / subsequent asset requests from the Tauri webview.
          res.setHeader(
            "Set-Cookie",
            `${DEV_GATE_COOKIE}=${DEV_GATE_TOKEN}; Path=/; HttpOnly; SameSite=Lax`,
          );
          return next();
        }
        if (hasCookie) {
          return next();
        }

        res.statusCode = 403;
        res.setHeader("Content-Type", "text/html; charset=utf-8");
        res.end(`<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8"/><title>hyper-grid</title>
<style>
  body{font-family:system-ui,sans-serif;max-width:36rem;margin:12vh auto;padding:1.5rem;line-height:1.55;color:#111}
  code{background:#ecfdf5;padding:.1rem .4rem;border-radius:4px}
  h1{color:#0f766e;font-size:1.35rem;margin:0 0 .75rem}
  h2{color:#0f766e;font-size:1.1rem;margin:1.75rem 0 .5rem;font-weight:600}
  p{margin:.45rem 0}
  .en{color:#4b5563}
</style>
</head>
<body>
  <h1>已关闭浏览器访问</h1>
  <p>hyper-grid 是桌面应用，不能在普通浏览器中使用。</p>
  <p>请在终端运行 <code>bunx tauri dev</code>，使用弹出的<strong>桌面窗口</strong>操作；不要打开此网址。</p>

  <h2 class="en">Browser access disabled</h2>
  <p class="en">hyper-grid is a desktop app and cannot run in a regular browser.</p>
  <p class="en">Run <code>bunx tauri dev</code> and use the desktop window that opens — do not open this URL in a browser.</p>
</body>
</html>`);
      });
    },
  };
}

export default defineConfig({
  plugins: [react(), tauriDevGate()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "esnext",
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});
