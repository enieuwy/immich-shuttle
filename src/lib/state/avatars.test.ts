import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("$lib/api", () => ({
  userProfileImage: vi.fn(),
}));

import { userProfileImage } from "$lib/api";
import type { AlbumUser } from "$lib/types";

const mockedFetch = vi.mocked(userProfileImage);

function user(id: string): AlbumUser {
  return { id, name: id, has_profile_image: true };
}

/** Deferred promises so tests control exactly when each fetch settles. */
function deferredFetches(): Map<string, (value: string | null) => void> {
  const resolvers = new Map<string, (value: string | null) => void>();
  mockedFetch.mockImplementation(
    (_profileId, userId) =>
      new Promise<string | null>((resolve) => {
        resolvers.set(userId, resolve);
      }),
  );
  return resolvers;
}

/**
 * Fresh module graph per test (avatars keeps module-level queue state), with
 * photo badges enabled unless a test opts out — fetching is gated on it.
 */
async function freshStore(display: "initials" | "photos" = "photos") {
  vi.resetModules();
  const theme = await import("./theme");
  theme.avatarDisplayState.setDisplay(display);
  const avatars = await import("./avatars");
  return { ...avatars, avatarDisplayState: theme.avatarDisplayState };
}

beforeEach(() => {
  mockedFetch.mockReset();
  localStorage.clear();
});

describe("avatarsState.prefetch", () => {
  it("fetches nothing in initials mode, replays on switch to photos", async () => {
    const { avatarsState, avatarDisplayState } = await freshStore("initials");
    deferredFetches();

    avatarsState.prefetch("p1", [user("u1"), user("u2")]);
    expect(mockedFetch).not.toHaveBeenCalled();

    avatarDisplayState.setDisplay("photos");
    expect(mockedFetch).toHaveBeenCalledTimes(2);
  });

  it("caps concurrent fetches and drains the queue as fetches settle", async () => {
    const { avatarsState } = await freshStore();
    const resolvers = deferredFetches();

    avatarsState.prefetch("p1", ["u1", "u2", "u3", "u4", "u5", "u6"].map(user));
    expect(mockedFetch).toHaveBeenCalledTimes(4);

    resolvers.get("u1")?.("data:image/png;base64,x");
    await vi.waitFor(() => expect(mockedFetch).toHaveBeenCalledTimes(5));
  });

  it("dedupes by key: re-prefetching the same users adds no requests", async () => {
    const { avatarsState } = await freshStore();
    deferredFetches();

    avatarsState.prefetch("p1", [user("u1")]);
    avatarsState.prefetch("p1", [user("u1")]);
    expect(mockedFetch).toHaveBeenCalledTimes(1);
  });

  it("evicts the departed profile on switch and ignores its late fetches", async () => {
    const { avatarsState, avatarKey } = await freshStore();
    const resolvers = deferredFetches();

    avatarsState.prefetch("p1", [user("u1")]);
    // Switch profiles while u1's fetch is still in flight.
    avatarsState.prefetch("p2", [user("u2")]);
    resolvers.get("u1")?.("data:image/png;base64,stale");
    resolvers.get("u2")?.("data:image/png;base64,fresh");

    await vi.waitFor(() => {
      const { images } = get(avatarsState);
      expect(images.get(avatarKey("p2", "u2"))).toBe("data:image/png;base64,fresh");
    });
    expect(get(avatarsState).images.has(avatarKey("p1", "u1"))).toBe(false);
  });

  it("caches a failed fetch as no-image instead of retrying forever", async () => {
    const { avatarsState, avatarKey } = await freshStore();
    mockedFetch.mockRejectedValue(new Error("boom"));

    avatarsState.prefetch("p1", [user("u1")]);
    await vi.waitFor(() => {
      expect(get(avatarsState).images.get(avatarKey("p1", "u1"))).toBeNull();
    });

    avatarsState.prefetch("p1", [user("u1")]);
    expect(mockedFetch).toHaveBeenCalledTimes(1);
  });
});
