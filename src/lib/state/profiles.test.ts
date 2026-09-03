import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

import * as api from "$lib/api";
import { errorsState } from "$lib/state/errors";
import type { Profile } from "$lib/types";

vi.mock("$lib/api", () => ({
  profilesList: vi.fn(async () => [
    {
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      lan_server_url: null,
      wan_server_url: null,
    },
  ]),
  profileDelete: vi.fn(async () => undefined),
  profileUpsert: vi.fn(async (input) => ({
    id: input.id ?? "new",
    display_name: input.display_name ?? "Immich User",
    server_url: input.server_url,
    lan_server_url: input.lan_server_url ?? null,
    wan_server_url: input.wan_server_url ?? null,
  })),
  profileValidate: vi.fn(async () => ({
    user_name: "Ellis",
    server_version: "1.120.0",
    is_compatible: true,
    warning: null,
  })),
}));

import { activeProfile, getProfilesSnapshot, profilesState } from "./profiles";

describe("profilesState", () => {
  beforeEach(async () => {
    for (const profile of getProfilesSnapshot().profiles) {
      await profilesState.deleteProfile(profile.id);
    }
  });
  it("loads profiles and sets first active profile", async () => {
    await profilesState.loadProfiles();
    expect(get(activeProfile)?.id).toBe("p1");
  });

  it("sets active profile manually", async () => {
    await profilesState.loadProfiles();
    profilesState.setActiveProfile("p1");
    expect(get(activeProfile)?.id).toBe("p1");
  });

  it("saves a new profile and makes it active", async () => {
    const saved = await profilesState.saveProfile({
      server_url: "https://new.example.com",
      display_name: "New User",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    expect(saved.id).toBeTruthy();
    expect(get(activeProfile)?.id).toBe(saved.id);
  });

  it("deletes active profile and clears active when no profiles left", async () => {
    for (const profile of getProfilesSnapshot().profiles) {
      await profilesState.deleteProfile(profile.id);
    }
    await profilesState.saveProfile({
      id: "only",
      server_url: "https://one.example.com",
      display_name: "Only",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    await profilesState.deleteProfile("only");
    expect(getProfilesSnapshot().profiles).toEqual([]);
    expect(getProfilesSnapshot().activeProfileId).toBeNull();
    expect(get(activeProfile)).toBeNull();
  });

  it("does not resurrect a profile deleted while an older load is still in flight", async () => {
    await profilesState.saveProfile({
      id: "del-me",
      server_url: "https://del.example.com",
      display_name: "Delete Me",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });

    // Gate this load so it resolves AFTER the delete below, simulating a read
    // issued before the mutation that completes after it.
    const gate = Promise.withResolvers<Profile[]>();
    vi.mocked(api.profilesList).mockReturnValueOnce(gate.promise);
    const stale = profilesState.loadProfiles();

    await profilesState.deleteProfile("del-me");

    // Stale response still contains the now-deleted profile — it must not win.
    gate.resolve([
      {
        id: "del-me",
        display_name: "Delete Me",
        server_url: "https://del.example.com",
        lan_server_url: null,
        wan_server_url: null,
      },
    ]);
    await stale;

    expect(getProfilesSnapshot().profiles.some((p) => p.id === "del-me")).toBe(false);
  });

  it("does not drop a profile saved while an older load is still in flight, nor move active off it", async () => {
    const gate = Promise.withResolvers<Profile[]>();
    vi.mocked(api.profilesList).mockReturnValueOnce(gate.promise);
    const stale = profilesState.loadProfiles();

    const saved = await profilesState.saveProfile({
      id: "save-me",
      server_url: "https://save.example.com",
      display_name: "Save Me",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });

    // Stale response predates the save — it must not erase it or the active id.
    gate.resolve([
      {
        id: "other",
        display_name: "Other",
        server_url: "https://other.example.com",
        lan_server_url: null,
        wan_server_url: null,
      },
    ]);
    await stale;

    const snapshot = getProfilesSnapshot();
    expect(snapshot.profiles.some((p) => p.id === saved.id)).toBe(true);
    expect(snapshot.activeProfileId).toBe(saved.id);
  });

  it("reports a failed load through errorsState and resolves instead of rejecting", async () => {
    vi.mocked(api.profilesList).mockRejectedValueOnce(new Error("network down"));

    await expect(profilesState.loadProfiles()).resolves.toBeUndefined();

    expect(get(errorsState).some((e) => e.message === "Could not load profiles.")).toBe(true);
    expect(getProfilesSnapshot().error).toBe("network down");
  });
});
