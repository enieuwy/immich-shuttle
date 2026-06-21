import { get, writable } from "svelte/store";

import { albumCreate, albumShareLink, albumShareUsers, albumsList, usersList } from "$lib/api";
import { errorsState } from "$lib/state/errors";
import type { Album, AlbumUser } from "$lib/types";

import { activeProfile } from "$lib/state/profiles";

type AlbumsState = {
  availableAlbums: Album[];
  selectedAlbumIds: string[];
  availableUsers: AlbumUser[];
  loading: boolean;
  error: string | null;
  shareLinkUrl: string | null;
};

const state = writable<AlbumsState>({
  availableAlbums: [],
  selectedAlbumIds: [],
  availableUsers: [],
  loading: false,
  error: null,
  shareLinkUrl: null,
});

export const albumsState = {
  subscribe: state.subscribe,
  async loadAlbums(query?: string) {
    const profile = get(activeProfile);
    if (!profile) {
      state.update((s) => ({ ...s, availableAlbums: [], availableUsers: [] }));
      return;
    }

    state.update((s) => ({ ...s, loading: true, error: null }));
    try {
      const [availableAlbums, availableUsers] = await Promise.all([
        albumsList(profile.id, query),
        usersList(profile.id),
      ]);
      state.update((s) => ({ ...s, availableAlbums, availableUsers, loading: false }));
    } catch (error) {
      errorsState.addError("Could not load albums.");
      state.update((s) => ({
        ...s,
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      }));
    }
  },
  selectAlbum(albumId: string) {
    state.update((s) => {
      if (s.selectedAlbumIds.includes(albumId)) {
        return s;
      }
      return { ...s, selectedAlbumIds: [...s.selectedAlbumIds, albumId] };
    });
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
  async createAlbum(name: string, shareUserIds: string[], createPublicLink: boolean) {
    const profile = get(activeProfile);
    if (!profile) {
      throw new Error("Select a profile before creating an album.");
    }
    try {
      const created = await albumCreate(profile.id, name);
      if (shareUserIds.length > 0) {
        await albumShareUsers(profile.id, created.id, shareUserIds);
      }
      let shareLinkUrl: string | null = null;
      if (createPublicLink) {
        const link = await albumShareLink(profile.id, created.id);
        shareLinkUrl = link.url;
      }
      state.update((s) => ({
        ...s,
        availableAlbums: [created, ...s.availableAlbums],
        selectedAlbumIds: [...s.selectedAlbumIds, created.id],
        shareLinkUrl,
      }));
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
