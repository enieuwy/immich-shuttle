import { invoke } from "@tauri-apps/api/core";
import type {
  Album,
  AlbumShareLink,
  AlbumUser,
  CaptureDate,
  ImportInput,
  ImportJob,
  ImportRecord,
  Profile,
  ProfileInput,
  RemovableDevice,
  ScanSummary,
  ServerInfo,
  ThumbResult,
} from "./types";

async function invokeCommand<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`${command} failed: ${message}`);
  }
}

export function profilesList(): Promise<Profile[]> {
  return invokeCommand<Profile[]>("profiles_list");
}

export function profileUpsert(input: ProfileInput): Promise<Profile> {
  return invokeCommand<Profile>("profile_upsert", { input });
}

export function profileDelete(id: string): Promise<void> {
  return invokeCommand<void>("profile_delete", { id });
}

export function profileValidate(url: string, apiKey: string): Promise<ServerInfo> {
  return invokeCommand<ServerInfo>("profile_validate", { url, apiKey });
}

export function albumsList(profileId: string, query?: string): Promise<Album[]> {
  return invokeCommand<Album[]>("albums_list", { profileId, query: query ?? null });
}

export function albumCreate(profileId: string, name: string): Promise<Album> {
  return invokeCommand<Album>("album_create", { profileId, name });
}

export type AlbumShareRole = "viewer" | "editor";

export function albumShareUsers(
  profileId: string,
  albumId: string,
  userIds: string[],
  role: AlbumShareRole,
): Promise<void> {
  return invokeCommand<void>("album_share_users", {
    profileId,
    albumId,
    userIds,
    role,
  });
}

export function albumShareLink(profileId: string, albumId: string): Promise<AlbumShareLink> {
  return invokeCommand<AlbumShareLink>("album_share_link", { profileId, albumId });
}

export function importStart(input: ImportInput): Promise<string> {
  return invokeCommand<string>("import_start", { input });
}

export function importCancel(jobId: string): Promise<void> {
  return invokeCommand<void>("import_cancel", { jobId });
}

export function importConfirmWipe(jobId: string, confirm: boolean): Promise<ImportJob> {
  return invokeCommand<ImportJob>("import_confirm_wipe", { jobId, confirm });
}

export function importRetry(jobId: string): Promise<string> {
  return invokeCommand<string>("import_retry", { jobId });
}

export function importDismiss(jobId: string): Promise<ImportJob[]> {
  return invokeCommand<ImportJob[]>("import_dismiss", { jobId });
}

export function importClearFinished(): Promise<ImportJob[]> {
  return invokeCommand<ImportJob[]>("import_clear_finished");
}

export function importListJobs(): Promise<ImportJob[]> {
  return invokeCommand<ImportJob[]>("import_list_jobs");
}

export function devicesListRemovable(): Promise<RemovableDevice[]> {
  return invokeCommand<RemovableDevice[]>("devices_list_removable");
}

export function usersList(profileId: string): Promise<AlbumUser[]> {
  return invokeCommand<AlbumUser[]>("users_list", { profileId });
}

export function getServerInfo(profileId: string): Promise<ServerInfo> {
  return invokeCommand<ServerInfo>("get_server_info", { profileId });
}

export function getLogsDir(): Promise<string> {
  return invokeCommand<string>("get_logs_dir");
}

export function openLogsDir(): Promise<void> {
  return invokeCommand<void>("open_logs_dir");
}

export function openInImmich(profileId: string, albumId?: string | null): Promise<void> {
  return invokeCommand<void>("open_in_immich", { profileId, albumId: albumId ?? null });
}

export function getRecentLogs(): Promise<string> {
  return invokeCommand<string>("get_recent_logs");
}

export function scanSourcesStream(paths: string[]): Promise<ScanSummary> {
  return invokeCommand<ScanSummary>("scan_sources_stream", { paths });
}

export function scanCancel(): Promise<void> {
  return invokeCommand<void>("scan_cancel");
}

export function previewThumbnails(paths: string[], token: number): Promise<ThumbResult[]> {
  return invokeCommand<ThumbResult[]>("preview_thumbnails", { paths, token });
}

export function previewDates(paths: string[], token: number): Promise<CaptureDate[]> {
  return invokeCommand<CaptureDate[]>("preview_dates", { paths, token });
}

export function previewCancel(token: number): Promise<void> {
  return invokeCommand<void>("preview_cancel", { token });
}

export function historyList(): Promise<ImportRecord[]> {
  return invokeCommand<ImportRecord[]>("history_list");
}

export function historyClear(): Promise<void> {
  return invokeCommand<void>("history_clear");
}

export function historySourceLastImport(sourcePaths: string[]): Promise<number | null> {
  return invokeCommand<number | null>("history_source_last_import", { sourcePaths });
}
