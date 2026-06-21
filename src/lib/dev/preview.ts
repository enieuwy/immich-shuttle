/**
 * Single entry point for the browser design preview, dynamically imported by
 * `main.ts` only in dev when no Tauri runtime is present. Keeping it behind one
 * dynamic import lets Vite split it (and all fixtures) out of production bundles.
 */
export { installTauriMock } from "$lib/dev/tauriMock";
export { seedStores } from "$lib/dev/scenarios";
