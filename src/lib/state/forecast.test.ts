import { describe, expect, it } from "vitest";

import { forecastProfileIdentity, nextForecastGeneration } from "./forecast";
import type { Profile } from "$lib/types";

function profile(overrides: Partial<Profile> = {}): Profile {
  return {
    id: "p1",
    display_name: "Home",
    server_url: "https://immich.example.com",
    lan_server_url: null,
    wan_server_url: null,
    ...overrides,
  };
}

describe("nextForecastGeneration", () => {
  it("never issues the same generation twice", () => {
    const issued = [
      nextForecastGeneration(),
      nextForecastGeneration(),
      nextForecastGeneration(),
    ];

    expect(new Set(issued).size).toBe(issued.length);
    expect(issued[1]).toBeGreaterThan(issued[0]);
    expect(issued[2]).toBeGreaterThan(issued[1]);
  });

  /**
   * ImportPreflight is mounted and unmounted with the panel around it, and its unmount
   * cleanup fires `forecastCancel(generation)` at a backend that cancels whichever request
   * currently holds that number. While the counter was component state it restarted at zero
   * on every mount, so the delayed cancel from the previous instance named the new
   * instance's first forecast and killed a live request the user had just asked for.
   */
  it("does not let a retired generation's delayed cancel kill the next mount's forecast", () => {
    // Stands in for the backend: one forecast at a time, and a cancel is honoured only when
    // it names the generation currently running.
    let running: number | null = null;
    const startForecast = () => {
      running = nextForecastGeneration();
      return running;
    };
    const cancelForecast = (generation: number) => {
      if (running === generation) running = null;
    };

    // Component lifetime A: one forecast, then the panel closes. Its cleanup cancel is
    // still in flight over IPC.
    const retired = startForecast();

    // Component lifetime B: a fresh instance, whose very first forecast must not collide
    // with anything lifetime A handed out.
    const live = startForecast();
    expect(live).not.toBe(retired);

    // A's cancel finally reaches the backend.
    cancelForecast(retired);

    expect(running).toBe(live);
  });
});

describe("forecastProfileIdentity", () => {
  /**
   * The stale forecast this defends against: the user edits the active profile's server URL
   * in place, so the id never changes, and the counts computed against the OLD host stay on
   * screen under the new one.
   */
  it("changes when the active profile's server URL is edited in place", () => {
    const before = forecastProfileIdentity(profile());
    const after = forecastProfileIdentity(profile({ server_url: "https://moved.example.com" }));

    expect(after).not.toBe(before);
  });

  it("changes when either alternate URL is edited in place", () => {
    const base = forecastProfileIdentity(profile());

    expect(forecastProfileIdentity(profile({ lan_server_url: "http://10.0.0.2:2283" }))).not.toBe(
      base,
    );
    expect(forecastProfileIdentity(profile({ wan_server_url: "https://wan.example.com" }))).not.toBe(
      base,
    );
  });

  it("changes when a different profile becomes active", () => {
    expect(forecastProfileIdentity(profile({ id: "p2" }))).not.toBe(
      forecastProfileIdentity(profile()),
    );
  });

  /**
   * The effect this feeds discards the shown forecast on every change, so an identity that
   * moved for a cosmetic edit would throw away a forecast that is still correct.
   */
  it("ignores fields that cannot change which server answered", () => {
    expect(forecastProfileIdentity(profile({ display_name: "Renamed" }))).toBe(
      forecastProfileIdentity(profile()),
    );
  });

  it("treats no active profile as its own identity", () => {
    expect(forecastProfileIdentity(null)).toBe("");
    expect(forecastProfileIdentity(undefined)).toBe("");
    expect(forecastProfileIdentity(profile())).not.toBe("");
  });
});
