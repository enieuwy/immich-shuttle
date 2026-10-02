import type { MediaFile } from "$lib/types";
import type { PreviewMetadata } from "$lib/previewApi";

export type MediaTypeFilter = "all" | "photo" | "video";
export type DatePreset = "all" | "7d" | "30d" | "year" | "custom";

export interface PreviewFilter {
  type: MediaTypeFilter;
  /** Inclusive lower bound, epoch seconds, or null for no lower bound. */
  fromEpoch: number | null;
  /** Inclusive upper bound, epoch seconds, or null for no upper bound. */
  toEpoch: number | null;
  /** Case-insensitive substring matched against the file name; "" = no filter. */
  nameQuery: string;
  /** Inclusive minimum size in bytes, or null for no lower bound. */
  minBytes: number | null;
  /** Inclusive maximum size in bytes, or null for no upper bound. */
  maxBytes: number | null;
  /** Selected extensions, with or without a leading dot; empty means all. */
  extensions?: ReadonlySet<string>;
  /** Case-insensitive camera make/model substring. */
  cameraQuery?: string;
  /** Active GPS predicates exclude metadata that has not loaded yet. */
  gps?: "all" | "with" | "without";
}

/** Local calendar date as "YYYY-MM-DD" (the value shape of <input type="date">). */
export function toYmd(date: Date): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, "0");
  const d = String(date.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

/**
 * Start-of-day epoch seconds for a "YYYY-MM-DD" string, or null if invalid.
 * Parsed as UTC (trailing `Z`) to match the backend, which builds capture
 * epochs by treating the EXIF wall-clock datetime as UTC (see
 * `civil_to_epoch`); a local parse here would shift photos near midnight into
 * the wrong day for browsers outside UTC.
 */
export function dayStartEpoch(ymd: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(ymd)) return null;
  const t = new Date(`${ymd}T00:00:00Z`).getTime();
  return Number.isNaN(t) ? null : Math.floor(t / 1000);
}

/** End-of-day (inclusive) UTC epoch seconds for a "YYYY-MM-DD" string, or null. */
export function dayEndEpoch(ymd: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(ymd)) return null;
  const t = new Date(`${ymd}T23:59:59.999Z`).getTime();
  return Number.isNaN(t) ? null : Math.floor(t / 1000);
}

/**
 * Date-string range for a preset, or null when the preset implies no bound
 * ("all") or a user-entered range ("custom"). "7d"/"30d" are inclusive of today.
 */
export function presetRange(
  preset: DatePreset,
  now: Date = new Date(),
): { from: string; to: string } | null {
  if (preset === "all" || preset === "custom") return null;
  const from = new Date(now);
  if (preset === "7d") {
    from.setDate(from.getDate() - 6);
  } else if (preset === "30d") {
    from.setDate(from.getDate() - 29);
  } else if (preset === "year") {
    from.setMonth(0, 1);
  }
  return { from: toYmd(from), to: toYmd(now) };
}

/**
 * Filter files by media type, capture date, filename, extension, size, camera,
 * and GPS. Active date predicates exclude unknown dates. Active metadata
 * predicates exclude rows that have not loaded; loaded absence matches only
 * "without GPS", never a camera query. All predicates combine with AND.
 */
export function filterFiles(
  files: MediaFile[],
  dates: Map<string, number | null>,
  filter: PreviewFilter,
  metadata?: ReadonlyMap<string, PreviewMetadata | null>,
): MediaFile[] {
  const hasDateBound = filter.fromEpoch !== null || filter.toEpoch !== null;
  const query = filter.nameQuery.trim().toLowerCase();
  const extensions = new Set(
    [...(filter.extensions ?? [])].map((extension) => extension.replace(/^\./, "").toLowerCase()),
  );
  const camera = filter.cameraQuery?.trim().toLowerCase() ?? "";
  return files.filter((f) => {
    if (filter.type === "photo" && f.is_video) return false;
    if (filter.type === "video" && !f.is_video) return false;
    if (query && !f.name.toLowerCase().includes(query)) return false;
    if (filter.minBytes !== null && f.size_bytes < filter.minBytes) return false;
    if (filter.maxBytes !== null && f.size_bytes > filter.maxBytes) return false;
    if (extensions.size && !extensions.has(f.extension.replace(/^\./, "").toLowerCase())) return false;
    const details = metadata?.get(f.path);
    if (camera && !details?.camera?.toLowerCase().includes(camera)) return false;
    if (filter.gps && filter.gps !== "all") {
      if (!metadata?.has(f.path)) return false;
      const hasGps = details?.gps_lat != null && details?.gps_lon != null;
      if (filter.gps === "with" && !hasGps) return false;
      if (filter.gps === "without" && hasGps) return false;
    }
    if (hasDateBound) {
      const captured = dates.get(f.path) ?? null;
      if (captured === null) return false;
      if (filter.fromEpoch !== null && captured < filter.fromEpoch) return false;
      if (filter.toEpoch !== null && captured > filter.toEpoch) return false;
    }
    return true;
  });
}
