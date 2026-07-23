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

/**
 * Curated dark-mode palettes. "indigo" is the classic base `.dark` look;
 * the others add a `palette-*` class whose CSS-var overrides live in app.css.
 * Light mode is unaffected by the palette.
 */
export type ThemePalette = "darkroom" | "indigo" | "ember";

const PALETTE_KEY = "immich-shuttle-palette";
const PALETTE_ORDER: ThemePalette[] = ["darkroom", "indigo", "ember"];

function getStoredPalette(): ThemePalette {
  try {
    const stored = localStorage.getItem(PALETTE_KEY);
    if (stored === "darkroom" || stored === "indigo" || stored === "ember") {
      return stored;
    }
  } catch {
  }
  return "darkroom";
}

function applyPalette(palette: ThemePalette): void {
  const root = document.documentElement;
  root.classList.remove("palette-darkroom", "palette-ember");
  if (palette !== "indigo") {
    root.classList.add(`palette-${palette}`);
  }
}

const paletteStore = writable<ThemePalette>(getStoredPalette());

applyPalette(get(paletteStore));

export const paletteState = {
  subscribe: paletteStore.subscribe,

  get palette(): ThemePalette {
    return get(paletteStore);
  },

  setPalette(palette: ThemePalette): void {
    paletteStore.set(palette);
    try {
      localStorage.setItem(PALETTE_KEY, palette);
    } catch {
    }
    applyPalette(palette);
  },

  cycle(): void {
    const idx = PALETTE_ORDER.indexOf(get(paletteStore));
    this.setPalette(PALETTE_ORDER[(idx + 1) % PALETTE_ORDER.length]);
  },
};

/**
 * How shared-album member badges render: colored initials (default — readable
 * at badge size) or the user's profile photo with a colored ring.
 */
export type AvatarDisplay = "initials" | "photos";

const AVATAR_DISPLAY_KEY = "immich-shuttle-avatar-display";

function getStoredAvatarDisplay(): AvatarDisplay {
  try {
    const stored = localStorage.getItem(AVATAR_DISPLAY_KEY);
    if (stored === "initials" || stored === "photos") {
      return stored;
    }
  } catch {
  }
  return "initials";
}

const avatarDisplayStore = writable<AvatarDisplay>(getStoredAvatarDisplay());

export const avatarDisplayState = {
  subscribe: avatarDisplayStore.subscribe,

  get display(): AvatarDisplay {
    return get(avatarDisplayStore);
  },

  setDisplay(display: AvatarDisplay): void {
    avatarDisplayStore.set(display);
    try {
      localStorage.setItem(AVATAR_DISPLAY_KEY, display);
    } catch {
    }
  },
};
