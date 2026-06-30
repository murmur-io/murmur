import { Routes } from "@angular/router";

export const routes: Routes = [
  // The chromeless floating-bar window — TOP-LEVEL, outside the shell layout
  // (no header/nav). Rendered in its own window via Rust.
  {
    path: "bar",
    loadComponent: () =>
      import("./features/bar/bar.component").then(
        (m) => m.FloatingBarComponent,
      ),
  },
  // Everything else renders INSIDE the shell layout. AppShellComponent supplies
  // the brand header / nav / page frame and a <router-outlet> for the page. It
  // is mounted by the ROUTER (ViewContainerRef.createComponent), which is what
  // makes this Tauri WKWebView style-resolve it — see AppShellComponent's class
  // comment for the FOUC rationale.
  {
    path: "",
    loadComponent: () =>
      import("./app-shell.component").then((m) => m.AppShellComponent),
    children: [
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
          import("./features/graph/graph.component").then(
            (m) => m.GraphComponent,
          ),
      },
      {
        path: "brain",
        loadComponent: () =>
          import("./features/brain/brain.component").then(
            (m) => m.BrainComponent,
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
    ],
  },
];
