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

export interface ImportInput {
  profile_id: string;
  source_paths: string[];
  album_ids: string[];
  keep_files: boolean;
  stack_raw_jpeg: boolean;
  stack_burst: boolean;
  date_range: string | null;
  concurrent_tasks: number | null;
}

export interface ImportJob {
  id: string;
  status: JobStatus;
  progress: JobProgress;
  error?: string | null;
  summary?: string | null;
  awaiting_wipe_confirmation: boolean;
  pending_wipe_count: number;
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
