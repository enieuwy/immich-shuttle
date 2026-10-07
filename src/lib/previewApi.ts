import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { BackendError } from "./backendErrors";
import type { ThumbResult } from "./types";

export interface PreviewMetadata {
  camera: string | null;
  lens: string | null;
  width: number | null;
  height: number | null;
  gps_lat: number | null;
  gps_lon: number | null;
  iso: number | null;
  exposure: string | null;
}

export interface PreviewMetadataRow {
  path: string;
  metadata: PreviewMetadata | null;
}

async function call<T>(command: string, args: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw new BackendError(command, error instanceof Error ? error.message : String(error), error);
  }
}

export function previewFullImage(path: string, token: number, maxPx = 2560): Promise<ThumbResult> {
  return call("preview_full_image", { path, maxPx, token });
}

export function previewMetadata(paths: string[], token: number): Promise<PreviewMetadataRow[]> {
  return call("preview_metadata", { paths, token });
}

export async function previewVideo(path: string, token: number): Promise<{ ticket: string; url: string }> {
  const ticket = await call<string>("preview_video", { path, token });
  return { ticket, url: convertFileSrc(ticket, "preview-media") };
}

export function previewVideoRelease(ticket: string): Promise<void> {
  return call("preview_video_release", { ticket });
}
