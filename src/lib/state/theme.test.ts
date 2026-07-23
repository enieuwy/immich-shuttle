import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";

import { avatarDisplayState, paletteState } from "./theme";

beforeEach(() => {
  localStorage.clear();
  paletteState.setPalette("darkroom");
});

describe("paletteState", () => {
  it("applies the palette class for non-default palettes and persists the choice", () => {
    paletteState.setPalette("ember");
    expect(document.documentElement.classList.contains("palette-ember")).toBe(true);
    expect(document.documentElement.classList.contains("palette-darkroom")).toBe(false);
    expect(localStorage.getItem("immich-shuttle-palette")).toBe("ember");
  });

  it("indigo is the bare .dark look — no palette class at all", () => {
    paletteState.setPalette("indigo");
    expect(document.documentElement.classList.contains("palette-darkroom")).toBe(false);
    expect(document.documentElement.classList.contains("palette-ember")).toBe(false);
    expect(localStorage.getItem("immich-shuttle-palette")).toBe("indigo");
  });

  it("cycles through all palettes and wraps around", () => {
    expect(get(paletteState)).toBe("darkroom");
    paletteState.cycle();
    expect(get(paletteState)).toBe("indigo");
    paletteState.cycle();
    expect(get(paletteState)).toBe("ember");
    paletteState.cycle();
    expect(get(paletteState)).toBe("darkroom");
    expect(document.documentElement.classList.contains("palette-darkroom")).toBe(true);
  });
});

describe("avatarDisplayState", () => {
  it("defaults to initials and persists an explicit choice", () => {
    expect(["initials", "photos"]).toContain(avatarDisplayState.display);
    avatarDisplayState.setDisplay("photos");
    expect(avatarDisplayState.display).toBe("photos");
    expect(localStorage.getItem("immich-shuttle-avatar-display")).toBe("photos");
    avatarDisplayState.setDisplay("initials");
    expect(localStorage.getItem("immich-shuttle-avatar-display")).toBe("initials");
  });
});
