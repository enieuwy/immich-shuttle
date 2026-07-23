import { get, writable } from "svelte/store";

import { albumCreate, albumShareLink, albumShareUsers, albumsList, usersList, type AlbumShareRole } from "$lib/api";
import { errorsState } from "$lib/state/errors";
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
};

// A new search supersedes any in-flight retry loop. Tauri invoke calls do not
// accept an AbortSignal, but the signal still prevents follow-up requests,
// retries, and state updates once the result is no longer relevant.
let loadGeneration = 0;
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
});

export const albumsState = {
  subscribe: state.subscribe,
  cancelLoad() {
    loadAbort?.abort();
    loadAbort = null;
    loadGeneration += 1;
    state.update((s) => ({ ...s, loading: false }));
  },
  async loadAlbums(query?: string) {
    loadAbort?.abort();
    const controller = new AbortController();
    loadAbort = controller;
    const { signal } = controller;
    const generation = ++loadGeneration;
    const isCurrent = () => !signal.aborted && generation === loadGeneration;
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
  ) {
    const profile = get(activeProfile);
    if (!profile) {
      throw new Error("Select a profile before creating an album.");
    }
    try {
      const created = await albumCreate(profile.id, name);
      // The album now exists on the server. Sharing and public-link creation are
      // best-effort follow-ups: if either fails we must still register the album
      // locally (otherwise it's orphaned server-side and desynced from the UI)
      // and tell the user precisely what didn't happen.
      const warnings: string[] = [];
      if (shareUserIds.length > 0) {
        try {
          await albumShareUsers(profile.id, created.id, shareUserIds, shareRole);
        } catch {
          warnings.push("could not share it with the selected users");
        }
      }
      let shareLinkUrl: string | null = null;
      if (createPublicLink) {
        try {
          const link = await albumShareLink(profile.id, created.id);
          shareLinkUrl = link.url;
        } catch {
          warnings.push("could not create a public link");
        }
      }
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
    } catch (error) {
      errorsState.addError("Could not create album.");
      throw error;
    }
  },
  clearShareLink() {
    state.update((s) => ({ ...s, shareLinkUrl: null }));
  },
};
