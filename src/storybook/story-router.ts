import { provideLocationMocks } from "@angular/common/testing";
import type { EnvironmentProviders, Provider } from "@angular/core";
import { provideRouter } from "@angular/router";

/**
 * A router for stories whose components carry `routerLink` or inject `Router`.
 *
 * The location is MOCKED, so a click on a link (or a component calling
 * `router.navigate`) never rewrites the Storybook iframe's own URL — a real
 * location would navigate `iframe.html` away and break the next reload. The
 * catch-all route lets every path the app uses (`/meetings`, `/notes/new`)
 * resolve to "nothing to render" instead of throwing NG04002.
 */
export function provideStoryRouter(): (Provider | EnvironmentProviders)[] {
  return [provideRouter([{ path: "**", children: [] }]), provideLocationMocks()];
}
