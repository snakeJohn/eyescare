import React from "react";
import ReactDOM from "react-dom/client";
import App, { BreakOverlay } from "./App";
import "./index.css";

async function boot() {
  let overlay = window.location.hash.startsWith("#break");
  if (!overlay && "__TAURI_INTERNALS__" in window) {
    try {
      const { getCurrentWebviewWindow } = await import("@tauri-apps/api/webviewWindow");
      overlay = getCurrentWebviewWindow().label === "break";
    } catch {
      /* ignore */
    }
  }
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      {overlay ? <BreakOverlay /> : <App />}
    </React.StrictMode>,
  );
}

void boot();
