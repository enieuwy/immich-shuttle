import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";

// Dev-only: when the app runs in a plain browser (no Tauri runtime injected),
// load the design preview — it mocks the Tauri backend with fixtures so the UI
// can be visually inspected and tuned. The dynamic import is required here: it
// keeps the mock + fixtures out of the production Tauri bundle (Vite drops this
// dead branch), which a hoisted static import could not.
const designPreview = import.meta.env.DEV && !("__TAURI_INTERNALS__" in window);
if (designPreview) {
  (await import("$lib/dev/preview")).installTauriMock();
}

// Real macOS Tauri window uses an overlay title bar (traffic lights only); flag
// it so the header can reserve space for the lights. Never set in the browser
// preview, where there is no native title bar.
if (!designPreview && navigator.userAgent.includes("Mac")) {
  document.documentElement.classList.add("titlebar-overlay");
}

const app = mount(App, {
  target: document.getElementById("app")!,
});

if (designPreview) {
  await (await import("$lib/dev/preview")).seedStores();
}

export default app;
