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
    path: "bar",
    loadComponent: () =>
      import("./features/bar/bar.component").then(
        (m) => m.FloatingBarComponent,
      ),
  },
  { path: "", pathMatch: "full", redirectTo: "record" },
  { path: "**", redirectTo: "record" },
];
