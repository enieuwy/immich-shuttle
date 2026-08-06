import { describe, expect, it } from "vitest";

import { createGeneration } from "./generation";

describe("createGeneration", () => {
  it("keeps only the newest generation current", () => {
    const gen = createGeneration();

    const first = gen.begin();
    expect(first()).toBe(true);

    const second = gen.begin();
    expect(first()).toBe(false);
    expect(second()).toBe(true);
  });

  it("rejects an older read that resolves after a newer one", async () => {
    const gen = createGeneration();
    let committed: string | null = null;

    // Explicit gates instead of delays: the point is the resolution ORDER, and
    // a timer would only approximate it.
    const staleGate = Promise.withResolvers<void>();
    const freshGate = Promise.withResolvers<void>();

    const read = async (value: string, gate: Promise<void>) => {
      const isCurrent = gen.begin();
      await gate;
      if (!isCurrent()) return;
      committed = value;
    };

    const stale = read("stale", staleGate.promise);
    const fresh = read("fresh", freshGate.promise);

    freshGate.resolve();
    await fresh;
    staleGate.resolve();
    await stale;

    expect(committed).toBe("fresh");
  });

  it("invalidate stops an in-flight read from committing", async () => {
    const gen = createGeneration();
    const gate = Promise.withResolvers<string[]>();
    let records: string[] = ["one", "two"];

    const load = async () => {
      const isCurrent = gen.begin();
      const rows = await gate.promise;
      if (!isCurrent()) return;
      records = rows;
    };
    const pending = load();

    // A destructive action lands while the read is still in flight.
    gen.invalidate();
    records = [];

    gate.resolve(["one", "two"]);
    await pending;

    expect(records).toEqual([]);
  });

  it("invalidate does not consume the next generation", () => {
    const gen = createGeneration();

    gen.invalidate();
    const next = gen.begin();

    expect(next()).toBe(true);
  });
});
