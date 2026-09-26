import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";

/**
 * Stands in for the Tauri v2 bridge so design-system components that reach
 * the Rust core through `IpcService` render in a plain browser. Built on
 * `@tauri-apps/api/mocks` (the official test seam), so the stories exercise the
 * SAME `IpcService` the app ships — only the transport underneath is fake.
 *
 * A story declares the commands it needs as `parameters.tauri`:
 *
 * ```ts
 * parameters: { tauri: { search_meetings: () => [hit] } }
 * ```
 *
 * An unmocked command REJECTS (and warns), which is what a missing backend
 * command does in the app too — components are expected to survive it.
 *
 * This mocks the TRANSPORT only. Payloads here are typed against the FE's own
 * models, so they cannot verify the backend contract (`angular-zoneless.md`
 * T6) — that is asserted on the Rust side.
 */
export type TauriHandler = (args: Record<string, unknown>) => unknown;
export type TauriHandlers = Record<string, TauriHandler>;

let handlers: TauriHandlers = {};

/** Replace the active command table (called per story by the preview decorator). */
export function setTauriHandlers(next: TauriHandlers | undefined): void {
  handlers = next ?? {};
}

export function installTauriMock(): void {
  mockWindows("main");
  mockIPC(
    (cmd, payload) => {
      const handler = handlers[cmd];
      if (!handler) {
        console.warn(`[storybook] no Tauri mock for "${cmd}"`);
        throw new Error(`Storybook: command "${cmd}" is not mocked`);
      }
      return handler((payload ?? {}) as Record<string, unknown>);
    },
    // `listen()` / `unlisten()` go through the event plugin; let the mock own
    // them so services that subscribe at startup register cleanly.
    { shouldMockEvents: true },
  );
}
