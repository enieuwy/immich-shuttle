import { describe, expect, it } from "vitest";

import { userDisplayNames } from "./users";

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
