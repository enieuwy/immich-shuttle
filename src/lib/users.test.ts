import { describe, expect, it } from "vitest";

import { avatarStyle, userDisplayNames } from "./users";

describe("userDisplayNames", () => {
  it("uses first names when they are unique", () => {
    expect(userDisplayNames([{ name: "Lauren Smith" }, { name: "John Doe" }])).toEqual([
      "Lauren",
      "John",
    ]);
  });

  it("disambiguates shared first names with last initials", () => {
    expect(userDisplayNames([{ name: "John Doe" }, { name: "John Smith" }])).toEqual([
      "John D",
      "John S",
    ]);
  });

  it("falls back to the full name when there is no last name to disambiguate", () => {
    expect(userDisplayNames([{ name: "John" }, { name: "John Smith" }])).toEqual([
      "John",
      "John S",
    ]);
  });

  it("handles single-token and padded names", () => {
    expect(userDisplayNames([{ name: "Cher" }, { name: "  Ada  Lovelace " }])).toEqual([
      "Cher",
      "Ada",
    ]);
  });
});

describe("avatarStyle", () => {
  it("maps Immich avatar color names onto the shared palette", () => {
    expect(avatarStyle({ id: "u1", avatar_color: "blue" }).bg).toBe("#3d6f98");
    expect(avatarStyle({ id: "u1", avatar_color: "pink" }).bg).toBe("#90556f");
  });

  it("uses dark text on yellow and amber for badge-size contrast", () => {
    expect(avatarStyle({ id: "u1", avatar_color: "yellow" }).fg).not.toBe("#ffffff");
    expect(avatarStyle({ id: "u1", avatar_color: "amber" }).fg).not.toBe("#ffffff");
    expect(avatarStyle({ id: "u1", avatar_color: "blue" }).fg).toBe("#ffffff");
  });

  it("hashes unknown or missing colors deterministically per user", () => {
    const a = avatarStyle({ id: "6c1f34e2-9b7a-4c1d-8f3e-000000000001" });
    const b = avatarStyle({ id: "6c1f34e2-9b7a-4c1d-8f3e-000000000001" });
    expect(a).toEqual(b);
    // Different ids should not all collapse onto one color: across a handful
    // of ids at least two distinct palette entries must appear.
    const seen = new Set(
      ["u-a", "u-b", "u-c", "u-d", "u-e", "u-f"].map((id) => avatarStyle({ id }).bg),
    );
    expect(seen.size).toBeGreaterThan(1);
  });

  it("prefers the named color over the hash fallback", () => {
    expect(avatarStyle({ id: "u-a", avatar_color: "gray" }).bg).toBe("#656970");
  });

  it("every palette pair meets WCAG 4.5:1 — initials render at 8-9px", () => {
    const luminance = (hex: string): number => {
      const [r, g, b] = [1, 3, 5]
        .map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
        .map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    const colors = [
      "primary", "pink", "red", "yellow", "blue",
      "green", "purple", "orange", "gray", "amber",
    ];
    for (const color of colors) {
      const { bg, fg } = avatarStyle({ id: "u1", avatar_color: color });
      const [hi, lo] = [luminance(bg), luminance(fg)].sort((a, b) => b - a);
      const ratio = (hi + 0.05) / (lo + 0.05);
      expect(ratio, `${color} (${fg} on ${bg})`).toBeGreaterThanOrEqual(4.5);
    }
  });
});
