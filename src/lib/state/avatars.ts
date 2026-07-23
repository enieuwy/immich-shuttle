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

// In-flight keys so a re-render or album reload never duplicates a fetch.
const inflight = new Set<string>();

export const avatarsState = {
  subscribe: state.subscribe,

  /**
   * Kick off profile-image fetches for any user that has one and is not yet
   * cached. Failures are cached as "no image" so a flaky server cannot cause
   * a retry storm from render-driven prefetches.
   */
  prefetch(profileId: string, users: AlbumUser[]): void {
    for (const user of users) {
      if (!user.has_profile_image) continue;
      const key = avatarKey(profileId, user.id);
      if (inflight.has(key) || get(state).images.has(key)) continue;
      inflight.add(key);
      void userProfileImage(profileId, user.id)
        .catch(() => null)
        .then((dataUrl) => {
          inflight.delete(key);
          state.update((s) => {
            const images = new Map(s.images);
            images.set(key, dataUrl);
            return { images };
          });
        });
    }
  },
};
