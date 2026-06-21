import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("$lib/api", () => ({
  profilesList: vi.fn(async () => []),
  profileDelete: vi.fn(async () => undefined),
  profileValidate: vi.fn(async () => ({
    user_name: "Ellis",
    server_version: "1.120.0",
    is_compatible: true,
    warning: null,
  })),
  profileUpsert: vi.fn(async (input) => ({
    id: input.id ?? "p1",
    display_name: input.display_name ?? "Ellis",
    server_url: input.server_url,
    lan_server_url: input.lan_server_url ?? null,
    wan_server_url: input.wan_server_url ?? null,
  })),
  albumsList: vi.fn(async () => [
    {
      id: "a1",
      album_name: "Family",
      shared_with: [],
    },
  ]),
  usersList: vi.fn(async () => []),
  albumCreate: vi.fn(async () => ({ id: "a2", album_name: "New", shared_with: [] })),
  albumShareUsers: vi.fn(async () => undefined),
  albumShareLink: vi.fn(async () => ({ url: "https://example.com/share/x" })),
}));

import * as api from "$lib/api";
import { profilesState } from "./profiles";
import { albumsState } from "./albums";

describe("albumsState", () => {
  it("selects and deselects albums", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    await albumsState.loadAlbums();
    albumsState.selectAlbum("a1");
    expect(get(albumsState).selectedAlbumIds).toContain("a1");
    albumsState.deselectAlbum("a1");
    expect(get(albumsState).selectedAlbumIds).not.toContain("a1");
  });

  it("does not duplicate selected album ids", async () => {
    albumsState.selectAlbum("a1");
    albumsState.selectAlbum("a1");
    const selected = get(albumsState).selectedAlbumIds.filter((id) => id === "a1");
    expect(selected).toHaveLength(1);
  });

  it("creates album and selects it", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    await albumsState.createAlbum("Holiday", ["u1"], true);
    expect(vi.mocked(api.albumCreate)).toHaveBeenCalled();
    expect(get(albumsState).selectedAlbumIds).toContain("a2");
  });

  it("stores the public share link after creating a linked album", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    vi.mocked(api.albumShareLink).mockClear();
    await albumsState.createAlbum("Holiday", ["u1"], true);
    expect(get(albumsState).shareLinkUrl).toBe("https://example.com/share/x");
    albumsState.clearShareLink();
    expect(get(albumsState).shareLinkUrl).toBeNull();
  });

  it("leaves the public share link empty when no link is created", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    await albumsState.createAlbum("Holiday", ["u1"], true);
    expect(get(albumsState).shareLinkUrl).toBe("https://example.com/share/x");

    vi.mocked(api.albumShareLink).mockClear();
    await albumsState.createAlbum("Private", [], false);
    expect(vi.mocked(api.albumShareLink)).not.toHaveBeenCalled();
    expect(get(albumsState).shareLinkUrl).toBeNull();
  });
});
