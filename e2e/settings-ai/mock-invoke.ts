import type { Page } from "@playwright/test";
import * as path from "path";

/**
 * The proven demo Tauri v2 mock (installs `window.__TAURI_INTERNALS__` with a full
 * command router + event plumbing that boots the real Angular app over fictional data).
 */
const BASE_MOCK = path.resolve(
  __dirname,
  "../../scripts/screenshots/mock-tauri.js",
);

/**
 * Boot the app under the demo Tauri mock, then override specific Tauri commands for
 * this test. Overrides run PAGE-SIDE (serialized to strings — they must be
 * self-contained: no closures over test-scope variables). Unknown commands fall
 * through to the demo mock's benign defaults, so the app always boots.
 *
 * `constants` is the escape hatch for that closure rule: each value is
 * JSON-serialized and replayed page-side as a constant-returning handler, so a
 * large fixture can live as a normal test-scope `const` instead of being
 * duplicated inline inside every override arrow.
 *
 * @example
 *   await mockTauri(page, { brain_posture: () => "hybrid" });
 *   await mockTauri(page, {}, { list_brain_models: REGISTRY });
 *   await mockTauri(page, {}, {}, ["murmur://privacy-event"]);
 *   await mockTauri(page, {}, {}, [], ["murmur://delayed-event"]);
 *   await mockTauri(page, {}, {}, [], [], { "murmur://event": [2] });
 *   await mockTauri(page, {}, {}, [], [], {}, { "murmur://event": [1] });
 *   await mockTauri(page, {}, {}, [], [], {}, {}, { "murmur://event": [1, 2] });
 */
export async function mockTauri(
  page: Page,
  overrides: Record<string, (args: any) => unknown> = {},
  constants: Record<string, unknown> = {},
  rejectedEventListeners: string[] = [],
  delayedEventListeners: string[] = [],
  delayedEventListenerOrdinals: Record<string, number[]> = {},
  rejectedEventListenerOrdinals: Record<string, number[]> = {},
  heldRejectedEventListenerOrdinals: Record<string, number[]> = {},
): Promise<void> {
  await page.addInitScript({ path: BASE_MOCK });
  const serialized = {
    ...Object.fromEntries(
      Object.entries(constants).map(([k, v]) => [
        k,
        `() => (${JSON.stringify(v)})`,
      ]),
    ),
    ...Object.fromEntries(
      Object.entries(overrides).map(([k, v]) => [k, v.toString()]),
    ),
  };
  await page.addInitScript(
    (config: {
      overrides: Record<string, string>;
      rejectedEventListeners: string[];
      delayedEventListeners: string[];
      delayedEventListenerOrdinals: Record<string, number[]>;
      rejectedEventListenerOrdinals: Record<string, number[]>;
      heldRejectedEventListenerOrdinals: Record<string, number[]>;
    }) => {
      const internals = (
        window as unknown as {
          __TAURI_INTERNALS__: {
            invoke: (c: string, a: unknown) => Promise<unknown>;
          };
        }
      ).__TAURI_INTERNALS__;
      const orig = internals.invoke.bind(internals);
      const names = Object.keys(config.overrides);
      const pendingEventListeners = new Map<string, Array<() => void>>();
      const releasedEventListeners = new Set<string>();
      const releasedHeldRejections = new Set<string>();
      const eventListenerCounts = new Map<string, number>();
      const eventUnregisterCounts = new Map<string, number>();
      const eventPluginInternals = (
        window as unknown as {
          __TAURI_EVENT_PLUGIN_INTERNALS__: {
            unregisterListener: (event: string, eventId: number) => void;
          };
        }
      ).__TAURI_EVENT_PLUGIN_INTERNALS__;
      const unregisterListener =
        eventPluginInternals.unregisterListener.bind(eventPluginInternals);
      const currentLocalDate = (daysAgo: number): string => {
        const date = new Date();
        date.setHours(12, 0, 0, 0);
        date.setDate(date.getDate() - daysAgo);
        const year = date.getFullYear();
        const month = String(date.getMonth() + 1).padStart(2, "0");
        const day = String(date.getDate()).padStart(2, "0");
        return `${year}-${month}-${day}`;
      };
      const refreshDemoAnalyticsWindow = (value: unknown): unknown => {
        if (
          typeof value !== "object" ||
          value === null ||
          !("perDay" in value) ||
          !Array.isArray(value.perDay)
        ) {
          return value;
        }
        const perDay = value.perDay;
        return {
          ...value,
          perDay: perDay.map((entry, index) =>
            typeof entry === "object" && entry !== null
              ? {
                  ...entry,
                  date: currentLocalDate(perDay.length - index - 1),
                }
              : entry,
          ),
        };
      };
      const rememberAskSourceTitles = (cmd: string, value: unknown): unknown => {
        const remember = (
          window as unknown as {
            __demoRememberAskSourceTitles?: (
              sources: Array<{ kind: string; id: string; title: string }>,
            ) => void;
          }
        ).__demoRememberAskSourceTitles;
        if (!remember) return value;
        if (cmd === "list_link_candidates" && Array.isArray(value)) {
          remember(
            value.filter(
              (row): row is { kind: string; id: string; title: string } =>
                typeof row === "object" &&
                row !== null &&
                typeof row.kind === "string" &&
                typeof row.id === "string" &&
                typeof row.title === "string",
            ),
          );
        } else if (cmd === "list_links" && Array.isArray(value)) {
          remember(
            value.flatMap((row) => {
              if (
                typeof row !== "object" ||
                row === null ||
                !("otherKind" in row) ||
                !("otherId" in row) ||
                !("otherTitle" in row)
              ) {
                return [];
              }
              return [
                {
                  kind: String(row.otherKind),
                  id: String(row.otherId),
                  title: String(row.otherTitle),
                },
              ];
            }),
          );
        } else if (cmd === "get_note" && typeof value === "object" && value) {
          const note = value as { id?: unknown; title?: unknown };
          if (typeof note.id === "string" && typeof note.title === "string") {
            remember([{ kind: "note", id: note.id, title: note.title }]);
          }
        } else if (
          cmd === "get_meeting_detail" &&
          typeof value === "object" &&
          value &&
          "meeting" in value
        ) {
          const meeting = (value as { meeting?: { id?: unknown; title?: unknown } })
            .meeting;
          if (
            meeting &&
            typeof meeting.id === "string" &&
            typeof meeting.title === "string"
          ) {
            remember([
              { kind: "meeting", id: meeting.id, title: meeting.title },
            ]);
          }
        }
        return value;
      };
      eventPluginInternals.unregisterListener = (
        event: string,
        eventId: number,
      ) => {
        eventUnregisterCounts.set(
          event,
          (eventUnregisterCounts.get(event) ?? 0) + 1,
        );
        unregisterListener(event, eventId);
      };
      (
        window as unknown as {
          __demoEventListenerRegistrationCount: (event: string) => number;
          __demoEventListenerUnregisterCount: (event: string) => number;
        }
      ).__demoEventListenerRegistrationCount = (event: string) =>
        eventListenerCounts.get(event) ?? 0;
      (
        window as unknown as {
          __demoEventListenerUnregisterCount: (event: string) => number;
        }
      ).__demoEventListenerUnregisterCount = (event: string) =>
        eventUnregisterCounts.get(event) ?? 0;
      (
        window as unknown as {
          __demoReleaseEventListeners: (event: string) => void;
        }
      ).__demoReleaseEventListeners = (event: string) => {
        releasedEventListeners.add(event);
        const pending = pendingEventListeners.get(event) ?? [];
        pendingEventListeners.delete(event);
        for (const release of pending) {
          release();
        }
      };
      (
        window as unknown as {
          __demoReleaseRejectedEventListeners: (event: string) => void;
        }
      ).__demoReleaseRejectedEventListeners = (event: string) => {
        releasedHeldRejections.add(event);
      };
      internals.invoke = (cmd: string, args: unknown) => {
        const event =
          typeof args === "object" && args !== null && "event" in args
            ? String((args as { event: unknown }).event)
            : "";
        const listenerOrdinal =
          cmd === "plugin:event|listen"
            ? (eventListenerCounts.get(event) ?? 0) + 1
            : 0;
        if (listenerOrdinal > 0) {
          eventListenerCounts.set(event, listenerOrdinal);
        }
        if (
          cmd === "plugin:event|listen" &&
          (config.rejectedEventListeners.includes(event) ||
            (config.rejectedEventListenerOrdinals[event] ?? []).includes(
              listenerOrdinal,
            ) ||
            ((config.heldRejectedEventListenerOrdinals[event] ?? []).includes(
              listenerOrdinal,
            ) &&
              !releasedHeldRejections.has(event)))
        ) {
          return Promise.reject(new Error(`mock listen rejected for ${event}`));
        }
        if (
          cmd === "plugin:event|listen" &&
          (config.delayedEventListeners.includes(event) ||
            (config.delayedEventListenerOrdinals[event] ?? []).includes(
              listenerOrdinal,
            )) &&
          !releasedEventListeners.has(event)
        ) {
          return new Promise((resolve, reject) => {
            const pending = pendingEventListeners.get(event) ?? [];
            pending.push(() => {
              orig(cmd, args).then(resolve, reject);
            });
            pendingEventListeners.set(event, pending);
          });
        }
        if (names.includes(cmd)) {
          const fn = new Function(
            "args",
            `return (${config.overrides[cmd]})(args);`,
          );
          return Promise.resolve(fn(args ?? {})).then((value) =>
            rememberAskSourceTitles(cmd, value),
          );
        }
        if (cmd === "get_analytics") {
          // The screenshot fixture has a fixed historical anchor. Keep only the
          // shared test helper's fallback window relative to today so the 30-day
          // chart remains non-uniform when the calendar advances.
          return orig(cmd, args).then(refreshDemoAnalyticsWindow);
        }
        return orig(cmd, args).then((value) =>
          rememberAskSourceTitles(cmd, value),
        );
      };
    },
    {
      overrides: serialized,
      rejectedEventListeners,
      delayedEventListeners,
      delayedEventListenerOrdinals,
      rejectedEventListenerOrdinals,
      heldRejectedEventListenerOrdinals,
    },
  );
}
