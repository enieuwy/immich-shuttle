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
  CaptureDate,
  ImportJob,
  ImportRecord,
  MediaFile,
  Profile,
  RemovableDevice,
  ScanResult,
  ServerInfo,
  ThumbResult,
} from "$lib/types";

export type Scenario =
  | "default"
  | "onboarding"
  | "importing"
  | "wipe"
  | "empty"
  | "cardinsert"
  | "preview";

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
  file_errors: [],
  profile_id: "p-home",
  album_id: "a-vacation",
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
          file_errors: [],
          profile_id: "p-home",
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
          file_errors: [],
          profile_id: "p-home",
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
          file_errors: [
            { file: "/Volumes/CANON_EOS/DCIM:100CANON/IMG_0412.CR3", reason: "Internal Server Error (500)" },
            { file: "/Volumes/CANON_EOS/DCIM:100CANON/IMG_0451.JPG", reason: "Unsupported media type" },
            { file: "/Volumes/CANON_EOS/DCIM:101CANON/MVI_0007.MOV", reason: "Connection reset by peer" },
            { file: "/Volumes/CANON_EOS/DCIM:101CANON/IMG_0533.HEIC", reason: "checksum mismatch" },
          ],
          profile_id: "p-home",
        },
      ];
  }
}

export function profilesForScenario(scenario: Scenario): Profile[] {
  return scenario === "onboarding" ? [] : profiles;
}

export const recentLogs = [
  "2026-06-22 08:11:02 import_start job_id=job-2b1e8d44 paths=1 albums=1",
  "2026-06-22 08:11:03 scan_complete photos=312 videos=0 size_mb=1840",
  "2026-06-22 08:14:51 import_complete job_id=job-2b1e8d44 status=Completed uploaded=308 total=312 errors=0",
  "2026-06-22 08:15:50 album_assign job_id=job-2b1e8d44 album=a-vacation assets=308 ok",
  "2026-06-22 09:02:11 import_start job_id=job-9d0c5a17 paths=2 albums=0",
  "2026-06-22 09:09:44 upload_error job_id=job-9d0c5a17 immich-go: connection reset by peer",
  "2026-06-22 09:09:45 import_complete job_id=job-9d0c5a17 status=Failed uploaded=410 total=540 errors=130",
].join("\n");

const PREVIEW_SPECS: { ext: string; video: boolean }[] = [
  { ext: ".jpg", video: false },
  { ext: ".jpg", video: false },
  { ext: ".jpg", video: false },
  { ext: ".cr3", video: false },
  { ext: ".heic", video: false },
  { ext: ".mov", video: true },
];

export const previewFiles: MediaFile[] = Array.from({ length: 48 }, (_, i) => {
  const spec = PREVIEW_SPECS[i % PREVIEW_SPECS.length];
  const n = String(1000 + i);
  return {
    path: `/Volumes/CANON_EOS/DCIM/100CANON/IMG_${n}${spec.ext}`,
    name: `IMG_${n}${spec.ext}`,
    extension: spec.ext,
    size_bytes: (spec.video ? 84 : 6) * 1024 * 1024,
    is_video: spec.video,
  };
});

export function scanResultForScenario(scenario: Scenario): ScanResult {
  if (scenario !== "preview") {
    return scanResult;
  }
  return {
    files: previewFiles,
    total_size_bytes: previewFiles.reduce((sum, f) => sum + f.size_bytes, 0),
    photo_count: previewFiles.filter((f) => !f.is_video).length,
    video_count: previewFiles.filter((f) => f.is_video).length,
    skipped_unreadable: 0,
  };
}

function thumbDataUrl(path: string): string {
  const seed = [...path].reduce((acc, ch) => acc + ch.charCodeAt(0), 0);
  const hue = (seed * 47) % 360;
  const svg =
    `<svg xmlns='http://www.w3.org/2000/svg' width='120' height='80'>` +
    `<rect width='120' height='80' fill='hsl(${hue},55%,55%)'/></svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

/** Mock backend: renders thumbnails for JPEG/PNG, placeholders for RAW/HEIC/video. */
export function thumbsForPaths(paths: string[]): ThumbResult[] {
  return paths.map((path) => {
    const lower = path.toLowerCase();
    const renderable = lower.endsWith(".jpg") || lower.endsWith(".jpeg") || lower.endsWith(".png");
    if (!renderable) {
      return { path, data_url: null, width: 0, height: 0 };
    }
    return { path, data_url: thumbDataUrl(path), width: 120, height: 80 };
  });
}

/** Mock backend: capture dates spread so Date order differs from Name order; videos have none. */
export function datesForPaths(paths: string[]): CaptureDate[] {
  const base = Date.UTC(2026, 5, 14, 12, 0, 0) / 1000;
  return paths.map((path, i) => {
    if (path.toLowerCase().endsWith(".mov")) {
      return { path, captured_at: null };
    }
    // Each later file in scan order is an hour newer → newest-first reverses name order.
    return { path, captured_at: base + i * 3600 };
  });
}
