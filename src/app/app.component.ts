import { ChangeDetectionStrategy, Component } from "@angular/core";
import { RouterLink, RouterLinkActive, RouterOutlet } from "@angular/router";

@Component({
  selector: "app-root",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterOutlet, RouterLink, RouterLinkActive],
  template: `
    <nav class="app-nav">
      <a routerLink="/record" routerLinkActive="active">Record</a>
      <a routerLink="/settings" routerLinkActive="active">Settings</a>
    </nav>
    <main class="app-main">
      <router-outlet></router-outlet>
    </main>
  `,
  styles: [
    `
      .app-nav {
        display: flex;
        gap: 1rem;
        padding: 0.75rem 1rem;
        border-bottom: 1px solid rgba(128, 128, 128, 0.3);
      }
      .app-nav a {
        text-decoration: none;
        color: inherit;
        opacity: 0.7;
      }
      .app-nav a.active {
        opacity: 1;
        font-weight: 600;
      }
      .app-main {
        padding: 1rem;
      }
    `,
  ],
})
export class AppComponent {}
