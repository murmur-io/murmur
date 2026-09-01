import { Routes } from "@angular/router";

export const routes: Routes = [
  // The chromeless floating-bar window — TOP-LEVEL, outside the shell layout
  // (no header/nav). Rendered in its own window via Rust.
  {
    path: "bar",
    loadComponent: () =>
      import("./features/bar/bar/bar.component").then(
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
      import("./app-shell/app-shell.component").then((m) => m.AppShellComponent),
    children: [
      {
        path: "record",
        loadComponent: () =>
          import("./features/record/record/record.component").then(
            (m) => m.RecordComponent,
          ),
      },
      {
        path: "settings",
        loadComponent: () =>
          import("./features/settings/settings/settings.component").then(
            (m) => m.SettingsComponent,
          ),
      },
      {
        path: "library",
        loadComponent: () =>
          import("./features/library/library/library.component").then(
            (m) => m.LibraryComponent,
          ),
      },
      {
        path: "meeting/:id",
        loadComponent: () =>
          import("./features/detail/detail/detail.component").then(
            (m) => m.DetailComponent,
          ),
      },
      {
        // Notes home — the [note-folder rail | note list] drill layout.
        path: "notes",
        loadComponent: () =>
          import("./features/notes/notes-home/notes-home.component").then(
            (m) => m.NotesHomeComponent,
          ),
      },
      {
        path: "trash",
        loadComponent: () =>
          import("./features/trash/trash/trash.component").then(
            (m) => m.TrashComponent,
          ),
      },
      {
        // Developer mode → Logs. The route stays reachable when developer mode
        // is off (a bookmark, a link in a bug report): the toggle decides what
        // the UI OFFERS, and the view reads nothing that needs guarding.
        path: "developer/logs",
        loadComponent: () =>
          import("./features/developer/logs/logs.component").then(
            (m) => m.LogsComponent,
          ),
      },
      {
        path: "reminders",
        loadComponent: () =>
          import("./features/reminders/reminders/reminders.component").then(
            (m) => m.RemindersComponent,
          ),
      },
      {
        path: "tasks",
        loadComponent: () =>
          import("./features/tasks/task-view/task-view.component").then(
            (m) => m.TaskViewComponent,
          ),
      },
      {
        path: "tasks/new",
        loadComponent: () =>
          import("./features/tasks/task-view/task-view.component").then(
            (m) => m.TaskViewComponent,
          ),
      },
      {
        path: "tasks/:id",
        loadComponent: () =>
          import("./features/tasks/task-view/task-view.component").then(
            (m) => m.TaskViewComponent,
          ),
      },
      {
        // Everything one container holds, paged per kind — where the sidebar tree'''s
        // container rows and its "Zobacz wszystkie" land. Without this route both
        // fell through the catch-all to /record, so clicking a project silently
        // opened the recorder.
        path: "container/:id",
        loadComponent: () =>
          import(
            "./features/workspace/container-view/container-view.component"
          ).then((m) => m.ContainerViewComponent),
      },
      {
        // Dashboards — the boards LIST. Deliberately NOT in
        // `TabRouteReuseStrategy`'s scope: a list route must be destroyed and
        // recreated so it always refetches, and its rows live in the root
        // `DashboardsService` so the remount is invisible (angular-zoneless §8).
        path: "dashboards",
        loadComponent: () =>
          import(
            "./features/dashboards/dashboards-home/dashboards-home.component"
          ).then((m) => m.DashboardsHomeComponent),
      },
      {
        // One board.
        path: "dashboards/:id",
        loadComponent: () =>
          import(
            "./features/dashboards/dashboard-view/dashboard-view.component"
          ).then((m) => m.DashboardViewComponent),
      },
      {
        // New-note gateway: creates a note then replaces the URL with /notes/:id.
        path: "notes/new",
        loadComponent: () =>
          import("./features/notes/note-editor/note-editor.component").then(
            (m) => m.NoteEditorComponent,
          ),
      },
      {
        // The note editor — drills like /meeting/:id (its own back-affordance).
        path: "notes/:id",
        loadComponent: () =>
          import("./features/notes/note-editor/note-editor.component").then(
            (m) => m.NoteEditorComponent,
          ),
      },
      {
        path: "analytics",
        loadComponent: () =>
          import("./features/analytics/analytics/analytics.component").then(
            (m) => m.AnalyticsComponent,
          ),
      },
      {
        path: "ask",
        loadComponent: () =>
          import("./features/ask/ask/ask.component").then((m) => m.AskComponent),
      },
      {
        path: "graph",
        loadComponent: () =>
          import("./features/graph/graph/graph.component").then(
            (m) => m.GraphComponent,
          ),
      },
      {
        path: "people",
        loadComponent: () =>
          import("./features/people/people/people.component").then(
            (m) => m.PeopleComponent,
          ),
      },
      {
        // The virtual "Shared Brains" Workspace. No longer a rail destination: it is
        // a ROW in the Workspaces sidebar, and this is the page behind it.
        path: "shared-brains",
        loadComponent: () =>
          import("./features/shared-brains/shared-brains.component").then(
            (m) => m.SharedBrainsComponent,
          ),
      },
      {
        // One RECEIVED container — a Workspace or folder somebody in the org shared.
        // Read-only structure at every access level; its owner keeps the tree.
        path: "shared/:orgId/:containerId",
        loadComponent: () =>
          import(
            "./features/shared-brains/shared-container-view/shared-container-view.component"
          ).then((m) => m.SharedContainerViewComponent),
      },
      {
        // Read-only org-brain item viewer — reached from an org-origin source
        // chip in Ask. Renders one decrypted OrgItemDetail (author + date + md).
        path: "org-item/:id",
        loadComponent: () =>
          import("./features/org/org-item-viewer/org-item-viewer.component").then(
            (m) => m.OrgItemViewerComponent,
          ),
      },
      {
        path: "brain",
        loadComponent: () =>
          import("./features/brain/brain/brain.component").then(
            (m) => m.BrainComponent,
          ),
      },
      {
        path: "onboarding",
        loadComponent: () =>
          import("./features/onboarding/onboarding/onboarding.component").then(
            (m) => m.OnboardingComponent,
          ),
      },
      {
        // First-run SHARING gateway (a shell child, router-mounted so the
        // packaged WKWebView style-resolves it — trap T4). Shown after the
        // onboarding gate when `!sharingChoiceMade && !accountStatus.loggedIn`.
        path: "welcome",
        loadComponent: () =>
          import("./features/sharing/sharing-gateway/sharing-gateway.component").then(
            (m) => m.SharingGatewayComponent,
          ),
      },
      { path: "", pathMatch: "full", redirectTo: "record" },
      { path: "**", redirectTo: "record" },
    ],
  },
];
