import { writable, get } from "svelte/store";

type ThemeMode = "system" | "light" | "dark";

const STORAGE_KEY = "immich-shuttle-theme";

function getStoredTheme(): ThemeMode {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === "light" || stored === "dark" || stored === "system") {
      return stored;
    }
  } catch {
  }
  return "system";
}

function applyTheme(mode: ThemeMode): void {
  const root = document.documentElement;
  if (mode === "dark") {
    root.classList.add("dark");
  } else if (mode === "light") {
    root.classList.remove("dark");
  } else {
    const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    root.classList.toggle("dark", prefersDark);
  }
}

const state = writable<ThemeMode>(getStoredTheme());

applyTheme(get(state));

if (typeof window !== "undefined") {
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (get(state) === "system") {
      applyTheme("system");
    }
  });
}

export const themeState = {
  subscribe: state.subscribe,

  get mode(): ThemeMode {
    return get(state);
  },

  setMode(mode: ThemeMode): void {
    state.set(mode);
    try {
      localStorage.setItem(STORAGE_KEY, mode);
    } catch {
    }
    applyTheme(mode);
  },

  cycle(): void {
    const current = get(state);
    const order: ThemeMode[] = ["system", "light", "dark"];
    const idx = order.indexOf(current);
    const next = order[(idx + 1) % order.length];
    this.setMode(next);
  },
};
