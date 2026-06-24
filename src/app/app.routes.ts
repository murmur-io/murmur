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
  { path: "", pathMatch: "full", redirectTo: "record" },
  { path: "**", redirectTo: "record" },
];
