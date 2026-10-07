import type { ImportRecord, Profile } from "$lib/types";

export interface HistoryTotals {
  runs: number;
  completed: number;
  failed: number;
  cancelled: number;
  unknown: number;
  incomplete: number;
  total: number;
  uploaded: number;
  duplicates: number;
  errors: number;
}
export interface ProfileHistoryTotals extends HistoryTotals {
  profileId: string;
  name: string;
}
export interface HistoryBucket {
  start: number;
  runs: number;
  uploaded: number;
  duplicates: number;
  errors: number;
}

const DAY_MS = 86_400_000;

function emptyTotals(): HistoryTotals {
  return { runs: 0, completed: 0, failed: 0, cancelled: 0, unknown: 0, incomplete: 0, total: 0, uploaded: 0, duplicates: 0, errors: 0 };
}

function addRecord(totals: HistoryTotals, record: ImportRecord) {
  totals.runs += 1;
  if (record.status === "completed") totals.completed += 1;
  else if (record.status === "failed") totals.failed += 1;
  else if (record.status === "cancelled") totals.cancelled += 1;
  else totals.unknown += 1;
  if (record.incomplete) totals.incomplete += 1;
  // Persisted Rust counters are unsigned integers. Reject invalid legacy
  // counters instead of letting one corrupt row make the entire chart NaN.
  for (const field of ["total", "uploaded", "duplicates", "errors"] as const) {
    const count = record[field];
    if (Number.isSafeInteger(count) && count >= 0) totals[field] += count;
  }
}

/** Aggregates only retained records. Does not infer transferred bytes. */
export function historyAnalytics(
  records: ImportRecord[],
  profiles: Profile[],
  profileId: string | null = null,
  interval: "day" | "week" = "day",
  now = Date.now(),
) {
  const totals = emptyTotals();
  const perProfile = new Map<string, ProfileHistoryTotals>();
  let skippedCounters = 0;
  let undatedRuns = 0;
  const today = Math.floor(now / DAY_MS) * DAY_MS;
  const weekday = new Date(today).getUTCDay();
  const currentStart = interval === "day" ? today : today - ((weekday + 6) % 7) * DAY_MS;
  const step = DAY_MS * (interval === "day" ? 1 : 7);
  const bucketCount = interval === "day" ? 30 : 12;
  const firstStart = currentStart - (bucketCount - 1) * step;
  const buckets: HistoryBucket[] = Array.from({ length: bucketCount }, (_, index) => ({
    start: firstStart + index * step, runs: 0, uploaded: 0, duplicates: 0, errors: 0,
  }));

  for (const record of records) {
    if (profileId !== null && record.profile_id !== profileId) continue;
    addRecord(totals, record);
    let profileTotals = perProfile.get(record.profile_id);
    if (!profileTotals) {
      const profile = profiles.find((p) => p.id === record.profile_id);
      profileTotals = { ...emptyTotals(), profileId: record.profile_id, name: profile?.display_name ?? `Removed profile (${record.profile_id})` };
      perProfile.set(record.profile_id, profileTotals);
    }
    addRecord(profileTotals, record);
    for (const field of ["total", "uploaded", "duplicates", "errors"] as const) {
      if (!Number.isSafeInteger(record[field]) || record[field] < 0) skippedCounters += 1;
    }
    if (!Number.isFinite(record.finished_at) || Number.isNaN(new Date(record.finished_at).getTime())) {
      undatedRuns += 1;
      continue;
    }
    const index = Math.floor((record.finished_at - firstStart) / step);
    if (index >= 0 && index < buckets.length) {
      const bucket = buckets[index];
      bucket.runs += 1;
      for (const field of ["uploaded", "duplicates", "errors"] as const) {
        if (Number.isSafeInteger(record[field]) && record[field] >= 0) bucket[field] += record[field];
      }
    }
  }
  return {
    totals, buckets,
    perProfile: [...perProfile.values()].sort((a, b) => a.name.localeCompare(b.name) || a.profileId.localeCompare(b.profileId)),
    skippedCounters, undatedRuns,
  };
}
