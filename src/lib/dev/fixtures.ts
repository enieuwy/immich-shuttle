/**
 * Realistic fixture data for the browser-based design preview.
 *
 * These are ONLY used when the app runs in a plain browser (no Tauri runtime),
 * so the full UI can be rendered and visually inspected without a real Immich
 * server. They are tree-shaken out of production Tauri builds.
 */
import type {
  Album,
  AlbumShareLink,
  AlbumUser,
  ImportJob,
  ImportRecord,
  Profile,
  RemovableDevice,
  ScanResult,
  ServerInfo,
} from "$lib/types";

export type Scenario = "default" | "onboarding" | "importing" | "wipe" | "empty";

export const PRESET_PATH = "/Volumes/CANON_EOS/DCIM";

export const profiles: Profile[] = [
  {
    id: "p-home",
    display_name: "Ada (home NAS)",
    server_url: "https://photos.adalovelace.dev",
    lan_server_url: "http://192.168.1.42:2283",
    wan_server_url: "https://photos.adalovelace.dev",
  },
  {
    id: "p-studio",
    display_name: "Studio archive",
    server_url: "https://immich.studio.example",
    lan_server_url: null,
    wan_server_url: null,
  },
];

export const serverInfo: ServerInfo = {
  user_name: "Ada Lovelace",
  server_version: "1.119.0",
  is_compatible: true,
  warning: null,
};

export const users: AlbumUser[] = [
  { id: "u-grace", name: "Grace Hopper", email: "grace@example.com" },
  { id: "u-alan", name: "Alan Turing", email: "alan@example.com" },
  { id: "u-katherine", name: "Katherine Johnson", email: "kat@example.com" },
];

export const albums: Album[] = [
  { id: "a-vacation", album_name: "Summer Vacation 2024", shared_with: [users[0]] },
  { id: "a-family", album_name: "Family", shared_with: [users[0], users[1]] },
  { id: "a-wedding", album_name: "Wedding — Grace & Alan", shared_with: [] },
  { id: "a-pets", album_name: "Pets", shared_with: [] },
  { id: "a-travel", album_name: "Travel — Iceland", shared_with: [users[2]] },
  { id: "a-screenshots", album_name: "Screenshots", shared_with: [] },
];

export const shareLink: AlbumShareLink = {
  url: "https://photos.adalovelace.dev/share/9f3c1d2e-demo",
};

export const devices: RemovableDevice[] = [
  {
    name: "CANON_EOS",
    mount_path: "/Volumes/CANON_EOS",
    total_space: 64 * 1024 * 1024 * 1024,
    available_space: 12 * 1024 * 1024 * 1024,
    has_dcim: true,
  },
  {
    name: "Untitled",
    mount_path: "/Volumes/Untitled",
    total_space: 256 * 1024 * 1024 * 1024,
    available_space: 240 * 1024 * 1024 * 1024,
    has_dcim: false,
  },
];

export const scanResult: ScanResult = {
  files: [],
  total_size_bytes: 8_540 * 1024 * 1024,
  photo_count: 1842,
  video_count: 57,
  skipped_unreadable: 3,
};

export const logsDir = "/Users/ada/Library/Logs/immich-shuttle";

export const lastImportMs = Date.now() - 1000 * 60 * 60 * 2;

export const historyRecords: ImportRecord[] = [
  {
    id: "job-2b1e8d44",
    started_at: lastImportMs,
    finished_at: lastImportMs + 1000 * 60 * 4,
    profile_id: "p-home",
    source_paths: [PRESET_PATH],
    album_ids: ["a-vacation"],
    status: "completed",
    total: 312,
    uploaded: 308,
    duplicates: 4,
    errors: 0,
  },
  {
    id: "job-9d0c5a17",
    started_at: Date.now() - 1000 * 60 * 60 * 26,
    finished_at: Date.now() - 1000 * 60 * 60 * 26 + 1000 * 60 * 9,
    profile_id: "p-home",
    source_paths: ["/Volumes/Untitled/Trip", "/Volumes/CANON_EOS/DCIM"],
    album_ids: [],
    status: "failed",
    total: 540,
    uploaded: 410,
    duplicates: 0,
    errors: 130,
  },
  {
    id: "job-7a1f3c02",
    started_at: Date.now() - 1000 * 60 * 60 * 24 * 5,
    finished_at: Date.now() - 1000 * 60 * 60 * 24 * 5 + 1000 * 60 * 2,
    profile_id: "p-studio",
    source_paths: ["/Users/ada/Pictures/Lightroom Export"],
    album_ids: ["a-wedding", "a-family"],
    status: "completed",
    total: 1203,
    uploaded: 1203,
    duplicates: 0,
    errors: 0,
  },
  {
    id: "job-5c8e1b90",
    started_at: Date.now() - 1000 * 60 * 60 * 24 * 9,
    finished_at: Date.now() - 1000 * 60 * 60 * 24 * 9 + 1000 * 30,
    profile_id: "p-home",
    source_paths: [PRESET_PATH],
    album_ids: [],
    status: "cancelled",
    total: 0,
    uploaded: 0,
    duplicates: 0,
    errors: 0,
  },
];

export function historyForScenario(scenario: Scenario): ImportRecord[] {
  return scenario === "onboarding" || scenario === "empty" ? [] : historyRecords;
}

const completedJob: ImportJob = {
  id: "job-2b1e8d44",
  status: "completed",
  progress: { total: 312, uploaded: 308, duplicates: 4, errors: 0 },
  error: null,
  summary: "Imported 308 items, skipped 4 duplicates.",
  awaiting_wipe_confirmation: false,
  pending_wipe_count: 0,
};

export function jobsForScenario(scenario: Scenario): ImportJob[] {
  switch (scenario) {
    case "importing":
      return [
        {
          id: "job-7f3a9c21",
          status: "running",
          progress: { total: 1899, uploaded: 1240, duplicates: 86, errors: 2 },
          error: null,
          summary: null,
          awaiting_wipe_confirmation: false,
          pending_wipe_count: 0,
        },
        completedJob,
      ];
    case "wipe":
      return [
        {
          id: "job-7f3a9c21",
          status: "running",
          progress: { total: 1899, uploaded: 1899, duplicates: 86, errors: 0 },
          error: null,
          summary: null,
          awaiting_wipe_confirmation: true,
          pending_wipe_count: 1813,
        },
      ];
    case "empty":
    case "onboarding":
      return [];
    default:
      return [
        completedJob,
        {
          id: "job-9d0c5a17",
          status: "failed",
          progress: { total: 540, uploaded: 410, duplicates: 0, errors: 130 },
          error: "Connection reset by server after 410 uploads.",
          summary: null,
          awaiting_wipe_confirmation: false,
          pending_wipe_count: 0,
        },
      ];
  }
}

export function profilesForScenario(scenario: Scenario): Profile[] {
  return scenario === "onboarding" ? [] : profiles;
}
