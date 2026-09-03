/**
 * The two process-wide facts a forecast depends on: which backend request it is, and which
 * server it was computed against.
 *
 * Both used to live inside ImportPreflight, which is wrong for the same reason in both
 * cases: a component instance is not the lifetime that matters. The backend outlives every
 * mount, and the profile a forecast was computed for can be edited without being replaced.
 */

import type { Profile } from "$lib/types";

// Strictly increasing for the life of the PROCESS, not the life of a component. A forecast
// is cancelled by naming its generation, and the backend cancels whatever currently holds
// that number. A counter that restarted at zero on every mount handed out numbers it had
// already given away: an unmounting instance fires cancel(1), the component remounts, its
// first forecast claims generation 1 again, and the delayed cancel kills the live request.
let issued = 0;

/** Claims the next generation. Every started forecast must take its own. */
export function nextForecastGeneration(): number {
  issued += 1;
  return issued;
}

/**
 * Everything about `profile` that decides which server a forecast asked, and therefore
 * which answer it got.
 *
 * The id alone is not it: editing the active profile's URL keeps the id and changes the
 * server, so a forecast tracked by id only stays on screen reporting counts from the
 * previous host. Which of the three URLs the backend resolves depends on reachability at
 * request time, which the frontend cannot observe -- so a change to any of them retires
 * the forecast.
 */
export function forecastProfileIdentity(profile: Profile | null | undefined): string {
  if (!profile) return "";
  return [
    profile.id,
    profile.server_url,
    profile.lan_server_url ?? "",
    profile.wan_server_url ?? "",
  ].join("\u0000");
}
