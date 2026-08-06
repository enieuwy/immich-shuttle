import { get, writable } from "svelte/store";

import { albumCreate, albumShareLink, albumShareUsers, albumsList, usersList, type AlbumShareRole } from "$lib/api";
import { avatarsState } from "$lib/state/avatars";
import { errorsState } from "$lib/state/errors";
import { createGeneration } from "$lib/state/generation";
import type { Album, AlbumUser } from "$lib/types";

import { activeProfile } from "$lib/state/profiles";

type AlbumsState = {
  availableAlbums: Album[];
  selectedAlbumIds: string[];
  availableUsers: AlbumUser[];
  loading: boolean;
  error: string | null;
  /** The active profile has no stored API key — prompt to add one instead of erroring. */
  missingApiKey: boolean;
  shareLinkUrl: string | null;
  /** Profile id whose albums are currently in availableAlbums, or null. */
  loadedProfileId: string | null;
  /** True while a create POST (plus share/link follow-ups) is in flight — gates the create button and blocks a second concurrent POST. */
  creating: boolean;
};

// A new search supersedes any in-flight retry loop. Tauri invoke calls do not
// accept an AbortSignal, but the signal still prevents follow-up requests,
// retries, and state updates once the result is no longer relevant. createAlbum
// also invalidates this so a list request started before a create can't commit
// afterward and drop what was just added; a profile switch invalidates it too,
// so a load for the old server can't commit after the switch.
const loadGeneration = createGeneration();
let loadAbort: AbortController | null = null;

function delay(ms: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.resolve();
  const { promise, resolve } = Promise.withResolvers<void>();
  const cancel = () => {
    clearTimeout(timer);
    resolve();
  };
  const timer = setTimeout(() => {
    signal.removeEventListener("abort", cancel);
    resolve();
  }, ms);
  signal.addEventListener("abort", cancel, { once: true });
  return promise;
}

const state = writable<AlbumsState>({
  availableAlbums: [],
  selectedAlbumIds: [],
  availableUsers: [],
  loading: false,
  error: null,
  missingApiKey: false,
  shareLinkUrl: null,
  loadedProfileId: null,
  creating: false,
});

// Album state is scoped to the active profile. activeProfileId changes
// synchronously on a profile switch, but the album reload is 150ms-debounced
// (AlbumSelector's search effect) — without this, the previous server's
// albums/selection/share-users would stay on screen until the debounce fires.
// This is the UX half: clear the stale view immediately and invalidate the
// load generation so a request issued for the old profile can't commit after
// the switch. queueState.startImport refusing to resolve an album unless
// loadedProfileId matches the active profile is the safety half.
let lastActiveProfileId: string | null | undefined;
activeProfile.subscribe((profile) => {
  const id = profile?.id ?? null;
  if (lastActiveProfileId === undefined) {
    // Skip the initial emission fired synchronously on subscribe — nothing to clear yet.
    lastActiveProfileId = id;
    return;
  }
  if (id === lastActiveProfileId) return;
  lastActiveProfileId = id;
  loadGeneration.invalidate();
  state.update((s) => ({
    ...s,
    selectedAlbumIds: [],
    availableAlbums: [],
    availableUsers: [],
    loadedProfileId: null,
  }));
});

export const albumsState = {
  subscribe: state.subscribe,
  cancelLoad() {
    loadAbort?.abort();
    loadAbort = null;
    loadGeneration.invalidate();
    state.update((s) => ({ ...s, loading: false }));
  },
  async loadAlbums(query?: string) {
    loadAbort?.abort();
    const controller = new AbortController();
    loadAbort = controller;
    const { signal } = controller;
    const isCurrentGeneration = loadGeneration.begin();
    const isCurrent = () => !signal.aborted && isCurrentGeneration();
    const profile = get(activeProfile);
    if (!profile) {
      if (isCurrent()) {
        state.update((s) => ({
          ...s,
          availableAlbums: [],
          availableUsers: [],
          loading: false,
          loadedProfileId: null,
        }));
      }
      return;
    }

    state.update((s) => ({ ...s, loading: true, error: null, missingApiKey: false }));

    // Reaching a LAN server triggers the macOS Local Network prompt, and macOS
    // denies the request that raises it. Retry a few times so we auto-recover the
    // moment the user grants access (or the server comes back) — no manual retry.
    const maxAttempts = 6;
    for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
      if (!isCurrent()) return;
      try {
        const availableAlbums = await albumsList(profile.id, query);
        if (!isCurrent()) return;
        // Non-admin Immich users get 403 from usersList; that must not block the
        // album list. Degrade the share-with-users picker to empty instead.
        let availableUsers: AlbumUser[] = [];
        if (!isCurrent()) return;
        try {
          availableUsers = await usersList(profile.id);
        } catch (usersError) {
          if (!isCurrent()) return;
          console.warn(
            "usersList failed (non-admin?):",
            usersError instanceof Error ? usersError.message : String(usersError),
          );
        }
        if (!isCurrent()) return;
        state.update((s) => ({
          ...s,
          availableAlbums,
          availableUsers,
          loading: false,
          error: null,
          loadedProfileId: profile.id,
        }));
        // Warm the avatar cache for everyone who can render as a badge:
        // album shared-with stacks plus the share-with-users picker.
        avatarsState.prefetch(profile.id, [
          ...availableAlbums.flatMap((album) => album.shared_with),
          ...availableUsers,
        ]);
        return;
      } catch (error) {
        if (!isCurrent()) return;
        const message = error instanceof Error ? error.message : String(error);
        // A missing key isn't an error to shout about — surface a CTA to add it.
        if (/No API key/i.test(message)) {
          state.update((s) => ({
            ...s,
            loading: false,
            availableAlbums: [],
            availableUsers: [],
            missingApiKey: true,
            error: null,
          }));
          return;
        }
        const isConnectionError =
          /error sending request|tcp connect|no route to host|connection refused|dns error|connect/i.test(
            message,
          );
        if (isConnectionError && attempt < maxAttempts) {
          if (!isCurrent()) return;
          await delay(2500, signal);
          if (!isCurrent()) return;
          continue;
        }
        console.warn("loadAlbums failed:", message);
        state.update((s) => ({
          ...s,
          loading: false,
          error: isConnectionError
            ? "Couldn't reach your server. Make sure it's running and reachable."
            : "Couldn't load albums.",
        }));
        return;
      }
    }
  },
  selectAlbum(albumId: string) {
    // Single-select: immich-go imports into one album (--into-album).
    state.update((s) => ({ ...s, selectedAlbumIds: [albumId] }));
  },
  deselectAlbum(albumId: string) {
    state.update((s) => ({
      ...s,
      selectedAlbumIds: s.selectedAlbumIds.filter((id) => id !== albumId),
    }));
  },
  clearSelection() {
    state.update((s) => ({ ...s, selectedAlbumIds: [] }));
  },
  async createAlbum(
    name: string,
    shareUserIds: string[],
    createPublicLink: boolean,
    shareRole: AlbumShareRole = "viewer",
  ): Promise<Album | undefined> {
    const profile = get(activeProfile);
    if (!profile) {
      throw new Error("Select a profile before creating an album.");
    }
    // albumCreate is a non-idempotent POST; a double-click or an impatient
    // second click before the first request lands would create a duplicate
    // album plus duplicate share/link follow-ups. Drop the second call
    // instead of queuing it — the button is also disabled while creating.
    if (get(state).creating) {
      return undefined;
    }
    const profileId = profile.id;
    state.update((s) => ({ ...s, creating: true }));
    try {
      const created = await albumCreate(profileId, name);
      // The album now exists on the server. Sharing and public-link creation are
      // best-effort follow-ups: if either fails we must still register the album
      // locally (otherwise it's orphaned server-side and desynced from the UI)
      // and tell the user precisely what didn't happen.
      const warnings: string[] = [];
      if (shareUserIds.length > 0) {
        try {
          await albumShareUsers(profileId, created.id, shareUserIds, shareRole);
        } catch {
          warnings.push("could not share it with the selected users");
        }
      }
      let shareLinkUrl: string | null = null;
      if (createPublicLink) {
        try {
          const link = await albumShareLink(profileId, created.id);
          shareLinkUrl = link.url;
        } catch {
          warnings.push("could not create a public link");
        }
      }
      // A create racing a profile switch must never publish into the store
      // the user has since switched away from — abandon the commit rather
      // than selecting a server-A album while server B is active.
      if (get(activeProfile)?.id !== profileId) {
        return undefined;
      }
      // loadAlbums may have started before this create and resolve after it;
      // without invalidating, that stale list response can commit afterward
      // and silently drop the album we're about to add.
      loadGeneration.invalidate();
      state.update((s) => ({
        ...s,
        availableAlbums: [created, ...s.availableAlbums],
        // Single-select: importing into the just-created album (--into-album).
        selectedAlbumIds: [created.id],
        shareLinkUrl,
      }));
      if (warnings.length > 0) {
        errorsState.addError(`Album "${name}" created, but ${warnings.join(" and ")}.`);
      }
      return created;
    } catch {
      // Already told the user via addError — AlbumSelector's caller has no
      // catch, so rethrowing would surface as an unhandled rejection for a
      // failure that was already reported.
      errorsState.addError("Could not create album.");
      return undefined;
    } finally {
      state.update((s) => ({ ...s, creating: false }));
    }
  },
  clearShareLink() {
    state.update((s) => ({ ...s, shareLinkUrl: null }));
  },
};
