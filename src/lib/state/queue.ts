import { get, writable } from "svelte/store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { importCancel, importConfirmWipe, importListJobs, importStart } from "$lib/api";
import { errorsState } from "$lib/state/errors";
import type { ImportJob } from "$lib/types";

import { importOptionsState } from "$lib/state/import-options";
import { albumsState } from "$lib/state/albums";
import { activeProfile } from "$lib/state/profiles";
import { sourceState } from "$lib/state/source";

type QueueState = {
  jobs: ImportJob[];
  loading: boolean;
  error: string | null;
};

const state = writable<QueueState>({
  jobs: [],
  loading: false,
  error: null,
});

let pollTimer: ReturnType<typeof setInterval> | null = null;
let progressUnlisten: UnlistenFn | null = null;

type ImportProgressEvent = {
  job_id: string;
  progress?: ImportJob["progress"];
  parsed_progress?: ImportJob["progress"];
};

async function refreshJobs() {
  try {
    const jobs = await importListJobs();
    state.update((s) => ({ ...s, jobs, error: null }));
  } catch (error) {
    errorsState.addError("Could not refresh import queue.");
    state.update((s) => ({ ...s, error: error instanceof Error ? error.message : String(error) }));
  }
}

export const queueState = {
  subscribe: state.subscribe,
  async loadJobs() {
    state.update((s) => ({ ...s, loading: true }));
    await refreshJobs();
    state.update((s) => ({ ...s, loading: false }));
  },
  startPolling() {
    if (pollTimer) {
      return;
    }
    if (!progressUnlisten) {
      void listen<ImportProgressEvent>("import-progress", (event) => {
        const payload = event.payload;
        if (!payload?.job_id) {
          return;
        }
        const progress = payload.parsed_progress ?? payload.progress;
        if (!progress) {
          return;
        }
        state.update((s) => ({
          ...s,
          jobs: s.jobs.map((job) =>
            job.id === payload.job_id ? { ...job, status: "running", progress } : job,
          ),
        }));
      }).then((unlisten) => {
        progressUnlisten = unlisten;
      });
    }
    pollTimer = setInterval(() => {
      void refreshJobs();
    }, 2000);
  },
  stopPolling() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
    if (progressUnlisten) {
      progressUnlisten();
      progressUnlisten = null;
    }
  },
  async startImport() {
    const profile = get(activeProfile);
    const source = get(sourceState);
    const options = get(importOptionsState);
    const albums = get(albumsState);

    if (!profile) {
      throw new Error("Select a profile before starting import.");
    }
    if (source.selectedPaths.length === 0) {
      throw new Error("Select a source before starting import.");
    }

    await importStart({
      profile_id: profile.id,
      source_paths: source.selectedPaths,
      album_ids: albums.selectedAlbumIds,
      keep_files: options.keepFiles,
      stack_raw_jpeg: options.stackRawJpeg,
      stack_burst: options.stackBurst,
    });
    await refreshJobs();
  },
  async cancelImport(jobId: string) {
    try {
      await importCancel(jobId);
      await refreshJobs();
    } catch (error) {
      errorsState.addError("Could not cancel import.");
      throw error;
    }
  },
  async confirmWipe(jobId: string, proceed: boolean) {
    try {
      await importConfirmWipe(jobId, proceed);
      await refreshJobs();
    } catch (error) {
      errorsState.addError("Could not complete wipe confirmation.");
      state.update((s) => ({ ...s, error: error instanceof Error ? error.message : String(error) }));
      throw error;
    }
  },
};
