export interface Profile {
  id: string;
  display_name: string;
  server_url: string;
  lan_server_url?: string | null;
  wan_server_url?: string | null;
}

export interface ProfileInput {
  id?: string | null;
  display_name?: string | null;
  server_url: string;
  lan_server_url?: string | null;
  wan_server_url?: string | null;
  api_key?: string | null;
}

export interface ServerInfo {
  user_name: string;
  server_version: string;
  is_compatible: boolean;
  warning?: string | null;
}

export interface AlbumUser {
  id: string;
  name: string;
  email?: string | null;
}

export interface Album {
  id: string;
  album_name: string;
  shared_with: AlbumUser[];
}

export interface AlbumShareLink {
  url: string;
}

export type JobStatus = "pending" | "running" | "completed" | "failed" | "cancelled";

export interface JobProgress {
  total: number;
  uploaded: number;
  duplicates: number;
  errors: number;
}

export type ImportOrganization = "single_album" | "folder_name" | "folder_path" | "folder_tags";

export interface ImportInput {
  profile_id: string;
  source_paths: string[];
  album_ids: string[];
  keep_files: boolean;
  stack_raw_jpeg: boolean;
  stack_burst: boolean;
  date_range: string | null;
  concurrent_tasks: number | null;
  select_files?: string[] | null;
  into_album?: string | null;
  organization?: ImportOrganization;
}

export interface FileError {
  file: string;
  reason: string;
}

export interface ImportJob {
  id: string;
  status: JobStatus;
  progress: JobProgress;
  error?: string | null;
  summary?: string | null;
  awaiting_wipe_confirmation: boolean;
  pending_wipe_count: number;
  file_errors: FileError[];
}

export interface RemovableDevice {
  name: string;
  mount_path: string;
  total_space: number;
  available_space: number;
  has_dcim: boolean;
}

export interface MediaFile {
  path: string;
  name: string;
  extension: string;
  size_bytes: number;
  is_video: boolean;
}

export interface ScanResult {
  files: MediaFile[];
  total_size_bytes: number;
  photo_count: number;
  video_count: number;
  skipped_unreadable: number;
}

/** Terminal outcome of a streamed scan (see scan_sources_stream). */
export interface ScanSummary {
  status: "complete" | "cancelled" | "timed_out";
  photo_count: number;
  video_count: number;
  total_size_bytes: number;
  skipped_unreadable: number;
}

/** Payload of each `scan-progress` event: an incremental batch plus cumulative totals. */
export interface ScanProgress {
  files: MediaFile[];
  photo_count: number;
  video_count: number;
  total_size_bytes: number;
  skipped_unreadable: number;
}

export interface ImportRecord {
  id: string;
  started_at: number;
  finished_at: number;
  profile_id: string;
  source_paths: string[];
  album_ids: string[];
  status: "completed" | "failed" | "cancelled";
  total: number;
  uploaded: number;
  duplicates: number;
  errors: number;
}

export interface ThumbResult {
  /** Source file path (echoed back). */
  path: string;
  /** Data URL of the generated thumbnail, or null when no backend could render it. */
  data_url: string | null;
  /** Thumbnail pixel dimensions; 0 when this is a placeholder. */
  width: number;
  height: number;
}

export interface CaptureDate {
  path: string;
  /** Capture time as epoch seconds, or null when unknown. */
  captured_at: number | null;
}
