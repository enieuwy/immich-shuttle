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
import { errorsState } from "./errors";
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

  it("flags a missing API key instead of raising an error", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    vi.mocked(api.albumsList).mockRejectedValueOnce(
      new Error("No API key found for profile: p1"),
    );
    await albumsState.loadAlbums();

    const s = get(albumsState);
    expect(s.missingApiKey).toBe(true);
    expect(s.error).toBeNull();
  });

  it("loads albums even when usersList fails (non-admin 403)", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    vi.mocked(api.albumsList).mockResolvedValueOnce([
      { id: "a1", album_name: "Family", shared_with: [] },
    ]);
    vi.mocked(api.usersList).mockRejectedValueOnce(new Error("403 Forbidden"));
    await albumsState.loadAlbums();

    const s = get(albumsState);
    expect(s.error).toBeNull();
    expect(s.availableAlbums.map((a) => a.id)).toContain("a1");
    expect(s.availableUsers).toEqual([]);
  });

  it("auto-retries a connection error and recovers", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    vi.useFakeTimers();
    vi.mocked(api.albumsList)
      .mockRejectedValueOnce(new Error("error sending request: tcp connect error -> No route to host"))
      .mockResolvedValueOnce([{ id: "a1", album_name: "Family", shared_with: [] }]);

    const pending = albumsState.loadAlbums();
    await vi.advanceTimersByTimeAsync(3000); // clear the retry backoff
    await pending;
    vi.useRealTimers();

    const s = get(albumsState);
    expect(s.error).toBeNull();
    expect(s.availableAlbums.map((a) => a.id)).toContain("a1");
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

    await albumsState.loadAlbums();
    albumsState.selectAlbum("a1");
    await albumsState.createAlbum("Holiday", ["u1"], true);
    expect(vi.mocked(api.albumCreate)).toHaveBeenCalled();
    // Single-select: the new album replaces any prior selection.
    expect(get(albumsState).selectedAlbumIds).toEqual(["a2"]);
  });

  it("still registers the album when sharing fails after creation", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    vi.mocked(api.albumShareUsers).mockRejectedValueOnce(new Error("share failed"));
    // Creation succeeds but the follow-up share fails: the album must not be
    // orphaned server-side, so createAlbum resolves and still registers/selects
    // it locally instead of throwing.
    const created = await albumsState.createAlbum("Holiday", ["u1"], false);
    // createAlbum resolves undefined only when it abandons the commit (reported
    // failure, superseded profile, or a create already in flight); a share
    // failure is none of those, so the album must still come back.
    expect(created).toBeDefined();
    expect(created?.id).toBe("a2");
    expect(get(albumsState).availableAlbums.some((a) => a.id === "a2")).toBe(true);
    expect(get(albumsState).selectedAlbumIds).toEqual(["a2"]);
  });

  it("forwards the share role (defaulting to viewer) to albumShareUsers", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    vi.mocked(api.albumShareUsers).mockClear();
    // Least-privilege default when the caller omits the role.
    await albumsState.createAlbum("Holiday", ["u1"], false);
    expect(vi.mocked(api.albumShareUsers)).toHaveBeenLastCalledWith("p1", "a2", ["u1"], "viewer");

    await albumsState.createAlbum("Team", ["u1", "u2"], false, "editor");
    expect(vi.mocked(api.albumShareUsers)).toHaveBeenLastCalledWith(
      "p1",
      "a2",
      ["u1", "u2"],
      "editor",
    );
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

  it("does not add or select an album whose create resolves after the active profile changed", async () => {
    await profilesState.saveProfile({
      id: "p1",
      display_name: "Ellis",
      server_url: "https://immich.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    await profilesState.saveProfile({
      id: "p2",
      display_name: "Other",
      server_url: "https://other.example.com",
      api_key: null,
      lan_server_url: null,
      wan_server_url: null,
    });
    profilesState.setActiveProfile("p1");

    const { promise, resolve } = Promise.withResolvers<{
      id: string;
      album_name: string;
      shared_with: never[];
    }>();
    vi.mocked(api.albumCreate).mockReturnValueOnce(promise);

    const beforeCount = get(albumsState).availableAlbums.length;
    const pending = albumsState.createAlbum("Race", [], false);
    // Profile switches away while the create is still in flight — the
    // eventual resolution must not publish server-A's album into server-B's
    // (now active) view.
    profilesState.setActiveProfile("p2");
    resolve({ id: "a-race", album_name: "Race", shared_with: [] });
    const created = await pending;

    expect(created).toBeUndefined();
    expect(get(albumsState).availableAlbums).toHaveLength(beforeCount);
    expect(get(albumsState).availableAlbums.some((a) => a.id === "a-race")).toBe(false);
    expect(get(albumsState).selectedAlbumIds).not.toContain("a-race");
  });

  it("issues exactly one albumCreate call for two back-to-back creates", async () => {
    profilesState.setActiveProfile("p1");
    vi.mocked(api.albumCreate).mockClear();

    const { promise, resolve } = Promise.withResolvers<{
      id: string;
      album_name: string;
      shared_with: never[];
    }>();
    vi.mocked(api.albumCreate).mockReturnValueOnce(promise);

    const first = albumsState.createAlbum("First", [], false);
    const second = albumsState.createAlbum("Second", [], false);
    expect(get(albumsState).creating).toBe(true);

    resolve({ id: "a-single", album_name: "First", shared_with: [] });
    const [firstResult, secondResult] = await Promise.all([first, second]);

    expect(vi.mocked(api.albumCreate)).toHaveBeenCalledTimes(1);
    expect(firstResult?.id).toBe("a-single");
    expect(secondResult).toBeUndefined();
    expect(get(albumsState).creating).toBe(false);
  });

  it("resolves (does not reject) and clears creating after a reported albumCreate failure", async () => {
    profilesState.setActiveProfile("p1");
    vi.mocked(api.albumCreate).mockRejectedValueOnce(new Error("boom"));
    const addError = vi.spyOn(errorsState, "addError");

    await expect(albumsState.createAlbum("Boom", [], false)).resolves.toBeUndefined();

    expect(addError).toHaveBeenCalledWith("Could not create album.");
    expect(get(albumsState).creating).toBe(false);
    addError.mockRestore();
  });

  it("clears availableAlbums, selectedAlbumIds, and loadedProfileId on a profile switch", async () => {
    profilesState.setActiveProfile("p1");
    await albumsState.loadAlbums();
    albumsState.selectAlbum("a1");
    expect(get(albumsState).availableAlbums.length).toBeGreaterThan(0);
    expect(get(albumsState).selectedAlbumIds).toContain("a1");
    expect(get(albumsState).loadedProfileId).toBe("p1");

    // A real id change (not a no-op re-emission of the same profile) must
    // clear server-A's view immediately rather than waiting out the 150ms
    // debounce on the next load.
    profilesState.setActiveProfile("p2");

    const s = get(albumsState);
    expect(s.availableAlbums).toEqual([]);
    expect(s.availableUsers).toEqual([]);
    expect(s.selectedAlbumIds).toEqual([]);
    expect(s.loadedProfileId).toBeNull();
  });
});
