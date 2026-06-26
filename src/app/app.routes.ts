import { Routes } from "@angular/router";

export const routes: Routes = [
  {
    path: "record",
    loadComponent: () =>
      import("./features/record/record.component").then(
        (m) => m.RecordComponent,
      ),
  },
  {
    path: "settings",
    loadComponent: () =>
      import("./features/settings/settings.component").then(
        (m) => m.SettingsComponent,
      ),
  },
  {
    path: "library",
    loadComponent: () =>
      import("./features/library/library.component").then(
        (m) => m.LibraryComponent,
      ),
  },
  {
    path: "meeting/:id",
    loadComponent: () =>
      import("./features/detail/detail.component").then(
        (m) => m.DetailComponent,
      ),
  },
  {
    path: "analytics",
    loadComponent: () =>
      import("./features/analytics/analytics.component").then(
        (m) => m.AnalyticsComponent,
      ),
  },
  {
    path: "ask",
    loadComponent: () =>
      import("./features/ask/ask.component").then((m) => m.AskComponent),
  },
  {
    path: "graph",
    loadComponent: () =>
      import("./features/graph/graph.component").then((m) => m.GraphComponent),
  },
  {
    path: "bar",
    loadComponent: () =>
      import("./features/bar/bar.component").then(
        (m) => m.FloatingBarComponent,
      ),
  },
  {
    path: "onboarding",
    loadComponent: () =>
      import("./features/onboarding/onboarding.component").then(
        (m) => m.OnboardingComponent,
      ),
  },
  { path: "", pathMatch: "full", redirectTo: "record" },
  { path: "**", redirectTo: "record" },
];
