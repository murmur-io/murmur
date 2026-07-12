import {
  ApplicationConfig,
  provideZonelessChangeDetection,
} from "@angular/core";
import { RouteReuseStrategy, provideRouter } from "@angular/router";
import { routes } from "./app.routes";
import { TabRouteReuseStrategy } from "./core/tab-route-reuse.strategy";

export const appConfig: ApplicationConfig = {
  providers: [
    provideZonelessChangeDetection(),
    provideRouter(routes),
    // Browser-style tabs (meeting/note "document" routes) need router-native
    // per-id detach/reattach caching — see tab-route-reuse.strategy.ts. Provide
    // the concrete class too (not just the token) so `TabsService` can inject
    // it directly for `evict()` on tab close; both resolve to ONE shared
    // instance (`useExisting`), so there's no DI cycle.
    TabRouteReuseStrategy,
    { provide: RouteReuseStrategy, useExisting: TabRouteReuseStrategy },
  ],
};
