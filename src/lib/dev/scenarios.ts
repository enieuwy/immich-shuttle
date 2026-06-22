/**
 * Scenario selection + store seeding for the browser design preview.
 *
 * The scenario is chosen via the `?scenario=` query param so individual UI
 * states can be screenshotted in isolation (e.g. `/?scenario=importing`).
 */
import * as fixtures from "$lib/dev/fixtures";
import type { Scenario } from "$lib/dev/fixtures";
import { albumsState } from "$lib/state/albums";
import { autoImportState } from "$lib/state/auto-import";
import { importOptionsState } from "$lib/state/import-options";
import { profilesState } from "$lib/state/profiles";
import { sourceState } from "$lib/state/source";

const SCENARIOS: readonly Scenario[] = [
  "default",
  "onboarding",
  "importing",
  "wipe",
  "empty",
  "cardinsert",
];

export function getScenario(): Scenario {
  const raw = new URLSearchParams(window.location.search).get("scenario");
  return SCENARIOS.includes(raw as Scenario) ? (raw as Scenario) : "default";
}

/**
 * Drives the frontend stores into a representative populated state after mount,
 * so cards like the source summary and album chips render with real-looking
 * content instead of their empty states.
 */
export async function seedStores(): Promise<void> {
  const scenario = getScenario();
  await profilesState.loadProfiles();

  if (scenario === "onboarding" || scenario === "empty") {
    return;
  }

  if (scenario === "cardinsert") {
    // Simulate a freshly-inserted DCIM card with auto-import enabled so the
    // "card detected" banner renders. Events don't fire in preview, so drive
    // the detector directly: baseline (empty) then the inserted device.
    autoImportState._reset();
    autoImportState.setEnabled(true);
    autoImportState.observe([]);
    autoImportState.observe([fixtures.devices[0]]);
    return;
  }

  await sourceState.selectSources([fixtures.PRESET_PATH]);
  await albumsState.loadAlbums();
  albumsState.selectAlbum("a-vacation");
  albumsState.selectAlbum("a-travel");

  if (scenario === "wipe") {
    importOptionsState.setKeepFiles(false);
  }
}
