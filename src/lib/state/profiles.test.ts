import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

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

import { activeProfile, profilesState } from "./profiles";

describe("profilesState", () => {
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
    await profilesState.saveProfile({
      id: "only",
      server_url: "https://one.example.com",
      display_name: "Only",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    await profilesState.deleteProfile("only");
    expect(get(activeProfile)?.id).not.toBe("only");
  });
});
