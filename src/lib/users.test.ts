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
    expect(avatarStyle({ id: "u1", avatar_color: "blue" }).bg).toBe("#3b82f6");
    expect(avatarStyle({ id: "u1", avatar_color: "pink" }).bg).toBe("#f472b6");
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
    expect(avatarStyle({ id: "u-a", avatar_color: "gray" }).bg).toBe("#4b5563");
  });
});
