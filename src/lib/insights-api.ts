import { invoke } from "@tauri-apps/api/core";
import { BackendError } from "$lib/backendErrors";
import type { Profile } from "$lib/types";

export interface BackupUiSettings {
  stackRawJpeg: boolean;
  stackBurst: boolean;
  concurrentTasks: number | null;
  keepGoingOnErrors: boolean;
  sessionTag: boolean;
  excludeExtensions: string[];
  theme: "system" | "light" | "dark";
  palette: "darkroom" | "indigo" | "ember";
  avatarDisplay: "initials" | "photos";
}

export interface MetadataBackup {
  format: "immich-shuttle-metadata";
  version: 1;
  profiles: Profile[];
  defaults: { keep_files_on_disk: boolean };
  ui_settings: BackupUiSettings;
}

export interface BackupImportResult {
  added: number;
  merged: number;
  needs_api_key: string[];
  ui_settings: BackupUiSettings | null;
  keep_files_on_disk: boolean | null;
}

export interface UpdateStatus {
  current_version: string;
  latest_version: string;
  update_available: boolean;
  releases_url: string;
}

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw new BackendError(name, error instanceof Error ? error.message : String(error), error);
  }
}

export function exportMetadata(path: string, uiSettings: BackupUiSettings): Promise<void> {
  return command("profiles_export", { path, uiSettings });
}
export function readMetadata(path: string): Promise<MetadataBackup> {
  return command("profiles_backup_read", { path });
}
export function importMetadata(backup: MetadataBackup, restoreSettings: boolean): Promise<BackupImportResult> {
  return command("profiles_import", { backup, restoreSettings });
}
export function getImportDefaults(): Promise<MetadataBackup["defaults"]> {
  return command("profiles_defaults");
}
export function checkForUpdates(): Promise<UpdateStatus> {
  return command("check_for_updates");
}
export function openProjectReleases(): Promise<void> {
  return command("open_project_releases");
}
