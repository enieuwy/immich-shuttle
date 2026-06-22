import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";

import { selectionState } from "./selection";

beforeEach(() => {
  selectionState.clear();
});

describe("selectionState", () => {
  it("toggles a path on and off", () => {
    selectionState.toggle("/a.jpg");
    expect(get(selectionState).selected.has("/a.jpg")).toBe(true);
    selectionState.toggle("/a.jpg");
    expect(get(selectionState).selected.has("/a.jpg")).toBe(false);
  });

  it("selectOnly replaces the whole selection", () => {
    selectionState.toggle("/a.jpg");
    selectionState.selectOnly(["/b.jpg", "/c.jpg"]);
    const sel = get(selectionState).selected;
    expect(sel.has("/a.jpg")).toBe(false);
    expect(sel.has("/b.jpg")).toBe(true);
    expect(sel.has("/c.jpg")).toBe(true);
    expect(sel.size).toBe(2);
  });

  it("clear empties the selection", () => {
    selectionState.selectOnly(["/a.jpg", "/b.jpg"]);
    selectionState.clear();
    expect(get(selectionState).selected.size).toBe(0);
  });

  it("has and paths reflect current selection", () => {
    selectionState.selectOnly(["/a.jpg", "/b.jpg"]);
    expect(selectionState.has("/a.jpg")).toBe(true);
    expect(selectionState.has("/z.jpg")).toBe(false);
    expect(selectionState.paths().sort()).toEqual(["/a.jpg", "/b.jpg"]);
  });

  it("replaces the Set instance on mutation for reactivity", () => {
    const before = get(selectionState).selected;
    selectionState.toggle("/a.jpg");
    const after = get(selectionState).selected;
    expect(after).not.toBe(before);
  });
});
