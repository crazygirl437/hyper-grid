import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { isTauriRuntime } from "./lib/api";
import "./i18n";
import "./styles.css";

function BrowserOnlyNotice() {
  return (
    <div className="browser-only">
      <h1>已关闭浏览器访问</h1>
      <p>hyper-grid 是桌面应用，不能在普通浏览器中使用。</p>
      <p>
        请运行 <code>bunx tauri dev</code>，在弹出的<strong>桌面窗口</strong>中操作；不要打开此网址。
      </p>
      <h2 className="browser-only-en-title">Browser access disabled</h2>
      <p className="browser-only-en">
        hyper-grid is a desktop app and cannot run in a regular browser.
      </p>
      <p className="browser-only-en">
        Run <code>bunx tauri dev</code> and use the desktop window that opens — do not open this URL in a
        browser.
      </p>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {isTauriRuntime() ? <App /> : <BrowserOnlyNotice />}
  </React.StrictMode>,
);
