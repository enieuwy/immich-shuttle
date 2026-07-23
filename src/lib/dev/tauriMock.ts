/**
 * Installs a fake `window.__TAURI_INTERNALS__` so the whole app can render in a
 * plain browser for visual design review. Every `invoke(...)` from
 * `@tauri-apps/api` and the dialog/opener/event plugins is answered with the
 * fixtures in `./fixtures`. Real Tauri builds always inject their own
 * `__TAURI_INTERNALS__` before our scripts run, so this never activates there;
 * the dynamic import in `main.ts` is also dev-gated and tree-shaken from
 * production bundles.
 */
import * as fixtures from "$lib/dev/fixtures";
import { getScenario } from "$lib/dev/scenarios";
import type { ProfileInput } from "$lib/types";

type InvokeArgs = Record<string, unknown> | undefined;

type TauriEvent = { event: string; id: number; payload: unknown };

// Registered `listen()` callbacks keyed by event name, so mocked commands can
// push events (e.g. the streamed scan's `scan-progress`) the way the real
// backend does. Real Tauri delivers events through this same callback channel.
const eventListeners = new Map<string, Set<number>>();

function emit(event: string, payload: unknown): void {
  const ids = eventListeners.get(event);
  if (!ids) return;
  const w = window as unknown as Record<string, (e: TauriEvent) => void>;
  for (const id of ids) {
    const cb = w[`_${id}`];
    if (typeof cb === "function") cb({ event, id, payload });
  }
}

function handle(cmd: string, args: InvokeArgs): unknown {
  // Plugin channels: events never fire in preview, dialogs/opener resolve no-op.
  if (cmd.startsWith("plugin:event|")) {
    if (cmd.endsWith("listen")) {
      const event = args?.event as string | undefined;
      const handler = args?.handler as number | undefined;
      if (event && typeof handler === "number") {
        const set = eventListeners.get(event) ?? new Set<number>();
        set.add(handler);
        eventListeners.set(event, set);
        return handler;
      }
      return Math.floor(Math.random() * 1e9);
    }
    if (cmd.endsWith("unlisten")) {
      const event = args?.event as string | undefined;
      const eventId = args?.eventId as number | undefined;
      if (event && typeof eventId === "number") eventListeners.get(event)?.delete(eventId);
      return undefined;
    }
    return undefined;
  }
  if (cmd.startsWith("plugin:dialog|")) {
    return fixtures.PRESET_PATH;
  }
  if (cmd.startsWith("plugin:")) {
    return undefined;
  }

  const scenario = getScenario();
  switch (cmd) {
    case "profiles_list":
      return fixtures.profilesForScenario(scenario);
    case "albums_list":
      return fixtures.albums;
    case "users_list":
      return fixtures.users;
    case "import_list_jobs":
      return fixtures.jobsForScenario(scenario);
    case "devices_list_removable":
      return fixtures.devices;
    case "scan_sources_stream": {
      const sr = fixtures.scanResultForScenario(scenario);
      // Mirror the real streamed scan: emit the files as a `scan-progress`
      // batch (the UI accumulates the grid from events), then return the
      // terminal summary the awaited command resolves with.
      emit("scan-progress", {
        files: sr.files,
        photo_count: sr.photo_count,
        video_count: sr.video_count,
        total_size_bytes: sr.total_size_bytes,
        skipped_unreadable: sr.skipped_unreadable,
      });
      return {
        status: "complete",
        photo_count: sr.photo_count,
        video_count: sr.video_count,
        total_size_bytes: sr.total_size_bytes,
        skipped_unreadable: sr.skipped_unreadable,
      };
    }
    case "preview_thumbnails":
      return fixtures.thumbsForPaths((args?.paths as string[]) ?? []);
    case "preview_dates":
      return fixtures.datesForPaths((args?.paths as string[]) ?? []);
    case "profile_validate":
    case "get_server_info":
      return fixtures.serverInfo;
    case "get_logs_dir":
      return fixtures.logsDir;
    case "album_share_link":
      return fixtures.shareLink;
    case "album_create": {
      const name = (args?.name as string) ?? "New album";
      return { id: `a-${Date.now()}`, album_name: name, shared_with: [] };
    }
    case "profile_upsert": {
      const input = (args?.input ?? {}) as ProfileInput;
      return {
        id: input.id ?? `p-${Date.now()}`,
        display_name: input.display_name ?? "New profile",
        server_url: input.server_url ?? "",
        lan_server_url: input.lan_server_url ?? null,
        wan_server_url: input.wan_server_url ?? null,
      };
    }
    case "import_start":
      return `job-${Date.now()}`;
    case "import_confirm_wipe":
      return { ...fixtures.jobsForScenario(scenario)[0], awaiting_wipe_confirmation: false };
    case "import_retry":
      return `job-${Date.now()}`;
    case "import_dismiss": {
      const jobId = args?.jobId as string | undefined;
      return fixtures.jobsForScenario(scenario).filter((job) => job.id !== jobId);
    }
    case "import_clear_finished":
      return fixtures
        .jobsForScenario(scenario)
        .filter((job) => job.status === "running" || job.status === "pending");
    case "history_list":
      return fixtures.historyForScenario(scenario);
    case "history_clear":
      return undefined;
    case "history_source_last_import":
      return scenario === "onboarding" || scenario === "empty" ? null : fixtures.lastImportMs;
    case "get_recent_logs":
      return scenario === "onboarding" || scenario === "empty" ? "" : fixtures.recentLogs;
    // Void commands: profile_delete, album_share_users, import_cancel, open_logs_dir, ...
    default:
      return undefined;
  }
}

export function installTauriMock(): void {
  const internals = {
    metadata: {
      currentWindow: { label: "main" },
      currentWebview: { windowLabel: "main", label: "main" },
    },
    plugins: {},
    invoke(cmd: string, args?: InvokeArgs) {
      return Promise.resolve(handle(cmd, args));
    },
    transformCallback(callback: (response: unknown) => void, once = false) {
      const id = Math.floor(Math.random() * 1e9);
      const prop = `_${id}`;
      Object.defineProperty(window, prop, {
        value: (response: unknown) => {
          if (once) {
            Reflect.deleteProperty(window, prop);
          }
          return callback(response);
        },
        writable: false,
        configurable: true,
      });
      return id;
    },
    convertFileSrc(filePath: string) {
      return filePath;
    },
  };

  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    value: internals,
    writable: true,
    configurable: true,
  });

  // Native confirm/alert/prompt would block a headless browser (the real
  // Tauri webview handles them fine). The wipe flow polls and calls
  // window.confirm(); stub them so the preview never hangs and the
  // awaiting-wipe state stays visible for screenshots.
  window.confirm = () => false;
  window.alert = () => {};
  window.prompt = () => null;

  console.info(`[design-preview] Tauri mock active — scenario "${getScenario()}"`);
}
