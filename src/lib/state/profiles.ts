import { derived, get, writable } from "svelte/store";

import { profileDelete, profileUpsert, profileValidate, profilesList } from "$lib/api";
import { errorsState } from "$lib/state/errors";
import type { Profile, ProfileInput, ServerInfo } from "$lib/types";

type ProfilesState = {
  profiles: Profile[];
  activeProfileId: string | null;
  loading: boolean;
  error: string | null;
};

const initialState: ProfilesState = {
  profiles: [],
  activeProfileId: null,
  loading: false,
  error: null,
};

const state = writable<ProfilesState>(initialState);

export const profilesState = {
  subscribe: state.subscribe,
  async loadProfiles() {
    state.update((s) => ({ ...s, loading: true, error: null }));
    try {
      const profiles = await profilesList();
      state.update((s) => ({
        ...s,
        profiles,
        activeProfileId: s.activeProfileId ?? profiles[0]?.id ?? null,
        loading: false,
      }));
    } catch (error) {
      errorsState.addError("Could not load profiles.");
      state.update((s) => ({
        ...s,
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      }));
      throw error;
    }
  },
  setActiveProfile(id: string) {
    state.update((s) => ({ ...s, activeProfileId: id }));
  },
  async saveProfile(input: ProfileInput) {
    const saved = await profileUpsert(input);
    state.update((s) => {
      const existingIndex = s.profiles.findIndex((p) => p.id === saved.id);
      const profiles = [...s.profiles];
      if (existingIndex >= 0) {
        profiles[existingIndex] = saved;
      } else {
        profiles.push(saved);
      }
      return { ...s, profiles, activeProfileId: saved.id };
    });
    return saved;
  },
  async deleteProfile(id: string) {
    await profileDelete(id);
    state.update((s) => {
      const profiles = s.profiles.filter((profile) => profile.id !== id);
      const activeProfileId = s.activeProfileId === id ? profiles[0]?.id ?? null : s.activeProfileId;
      return { ...s, profiles, activeProfileId };
    });
  },
  async validateProfile(url: string, apiKey: string): Promise<ServerInfo> {
    try {
      return await profileValidate(url, apiKey);
    } catch (error) {
      errorsState.addError("Profile validation failed.");
      throw error;
    }
  },
};

export const activeProfile = derived(state, ($state) =>
  $state.profiles.find((profile) => profile.id === $state.activeProfileId) ?? null,
);

export function getProfilesSnapshot() {
  return get(state);
}
