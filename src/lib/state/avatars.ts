import { get, writable } from "svelte/store";

import { userProfileImage } from "$lib/api";
import type { AlbumUser } from "$lib/types";

type AvatarsState = {
  /** Data URL per avatar key; null = fetched, the user has no image. */
  images: Map<string, string | null>;
};

export function avatarKey(profileId: string, userId: string): string {
  return `${profileId}:${userId}`;
}

const state = writable<AvatarsState>({ images: new Map() });

// Keys that are queued or actively fetching, so a re-render or album reload
// never duplicates a request.
const inflight = new Set<string>();

// Fetches run through a small pool: a server with a large user directory must
// not trigger its entire avatar set as one concurrent burst.
const MAX_CONCURRENT_FETCHES = 4;
let activeFetches = 0;
const pendingQueue: { key: string; profileId: string; userId: string }[] = [];

// Only the active profile's avatars are retained; switching profiles evicts
// the old profile's data URLs so memory tracks the working set, not every
// profile ever visited this session.
let retainedProfileId: string | null = null;

function pump(): void {
  while (activeFetches < MAX_CONCURRENT_FETCHES && pendingQueue.length > 0) {
    const job = pendingQueue.shift();
    if (!job) break;
    activeFetches += 1;
    void userProfileImage(job.profileId, job.userId)
      .catch(() => null)
      .then((dataUrl) => {
        activeFetches -= 1;
        inflight.delete(job.key);
        // A fetch that raced a profile switch must not repopulate the cache
        // with the departed profile's data.
        if (job.profileId === retainedProfileId) {
          state.update((s) => {
            const images = new Map(s.images);
            images.set(job.key, dataUrl);
            return { images };
          });
        }
        pump();
      });
  }
}

export const avatarsState = {
  subscribe: state.subscribe,

  /**
   * Queue profile-image fetches for any user that has one and is not yet
   * cached. Failures are cached as "no image" so a flaky server cannot cause
   * a retry storm from render-driven prefetches.
   */
  prefetch(profileId: string, users: AlbumUser[]): void {
    if (retainedProfileId !== profileId) {
      retainedProfileId = profileId;
      // Drop queued work for other profiles (their inflight marks too) and
      // evict cached entries that don't belong to the new profile.
      for (const job of pendingQueue) {
        inflight.delete(job.key);
      }
      pendingQueue.length = 0;
      const prefix = `${profileId}:`;
      state.update((s) => {
        const images = new Map<string, string | null>();
        for (const [key, value] of s.images) {
          if (key.startsWith(prefix)) {
            images.set(key, value);
          }
        }
        return { images };
      });
    }
    for (const user of users) {
      if (!user.has_profile_image) continue;
      const key = avatarKey(profileId, user.id);
      if (inflight.has(key) || get(state).images.has(key)) continue;
      inflight.add(key);
      pendingQueue.push({ key, profileId, userId: user.id });
    }
    pump();
  },
};
