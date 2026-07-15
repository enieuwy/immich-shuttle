import type { MediaFile } from "$lib/types";

export type MediaTypeFilter = "all" | "photo" | "video";
export type DatePreset = "all" | "7d" | "30d" | "year" | "custom";

export interface PreviewFilter {
  type: MediaTypeFilter;
  /** Inclusive lower bound, epoch seconds, or null for no lower bound. */
  fromEpoch: number | null;
  /** Inclusive upper bound, epoch seconds, or null for no upper bound. */
  toEpoch: number | null;
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
 * Filter `files` by media type and capture-date window. Files whose capture date
 * is unknown are excluded whenever a date bound is active (we can't confirm they
 * fall in range), and included otherwise.
 */
export function filterFiles(
  files: MediaFile[],
  dates: Map<string, number | null>,
  filter: PreviewFilter,
): MediaFile[] {
  const hasDateBound = filter.fromEpoch !== null || filter.toEpoch !== null;
  return files.filter((f) => {
    if (filter.type === "photo" && f.is_video) return false;
    if (filter.type === "video" && !f.is_video) return false;
    if (hasDateBound) {
      const captured = dates.get(f.path) ?? null;
      if (captured === null) return false;
      if (filter.fromEpoch !== null && captured < filter.fromEpoch) return false;
      if (filter.toEpoch !== null && captured > filter.toEpoch) return false;
    }
    return true;
  });
}
