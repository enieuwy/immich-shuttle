import { derived, get, writable } from "svelte/store";

import { profileDelete, profileUpsert, profileValidate, profilesList } from "$lib/api";
import { errorsState } from "$lib/state/errors";
import { createGeneration } from "$lib/state/generation";
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

// Guards loadProfiles against out-of-order completion: a load started before a
// save/delete can otherwise resolve after it and overwrite the newer state
// (deleted profile comes back, saved profile vanishes, active profile drifts).
const loads = createGeneration();

export const profilesState = {
  subscribe: state.subscribe,
  async loadProfiles() {
    const isCurrent = loads.begin();
    state.update((s) => ({ ...s, loading: true, error: null }));
    try {
      const profiles = await profilesList();
      if (!isCurrent()) return;
      state.update((s) => ({
        ...s,
        profiles,
        activeProfileId: s.activeProfileId ?? profiles[0]?.id ?? null,
        loading: false,
      }));
    } catch (error) {
      if (!isCurrent()) return;
      // Policy: App.svelte's onMount runs this as
      // `void profilesState.loadProfiles().then(...)` with no `.catch`. The user
      // is already told via the toast below, so re-throwing would only produce
      // an unhandled rejection at startup — resolve instead and let `error` in
      // state drive any inline UI.
      errorsState.addError("Could not load profiles.");
      state.update((s) => ({
        ...s,
        loading: false,
        error: error instanceof Error ? error.message : String(error),
      }));
    }
  },
  setActiveProfile(id: string) {
    state.update((s) => ({ ...s, activeProfileId: id }));
  },
  async saveProfile(input: ProfileInput) {
    const saved = await profileUpsert(input);
    // Supersede in-flight loads: a read issued before this save must not land
    // after it and drop the profile just committed. saveProfile itself keeps
    // throwing on failure — its caller (the profile dialog) needs the
    // rejection to keep the dialog open, so it reports the error, not us.
    loads.invalidate();
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
    // Supersede in-flight loads: a read issued before this delete must not
    // land after it and resurrect the profile just removed.
    loads.invalidate();
    state.update((s) => {
      const profiles = s.profiles.filter((profile) => profile.id !== id);
      const activeProfileId = s.activeProfileId === id ? profiles[0]?.id ?? null : s.activeProfileId;
      return { ...s, profiles, activeProfileId };
    });
  },
  async validateProfile(url: string, apiKey: string): Promise<ServerInfo> {
    // No addError here: the only caller (ProfileEditor's "Test connection")
    // catches the rejection and renders its own inline Alert from the error
    // message, so a toast would just duplicate what's already on screen. It
    // needs the throw to know validation failed, so this keeps rejecting.
    return await profileValidate(url, apiKey);
  },
};

export const activeProfile = derived(state, ($state) =>
  $state.profiles.find((profile) => profile.id === $state.activeProfileId) ?? null,
);

export function getProfilesSnapshot() {
  return get(state);
}
