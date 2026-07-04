import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { startWith } from "rxjs";
import { NavHistoryService } from "../../core/nav-history.service";
import { SettingsStore } from "./settings.store";
import { SettingsAppearanceSectionComponent } from "./sections/settings-appearance-section.component";
import { SettingsGeneralSectionComponent } from "./sections/settings-general-section.component";
import { SettingsTranscriptionSectionComponent } from "./sections/settings-transcription-section.component";
import { SettingsAudioSectionComponent } from "./sections/settings-audio-section.component";
import { SettingsStorageSectionComponent } from "./sections/settings-storage-section.component";
import { SettingsNotesSectionComponent } from "./sections/settings-notes-section.component";
import { SettingsAiSectionComponent } from "./sections/settings-ai-section.component";
import { SettingsConnectorsSectionComponent } from "./sections/settings-connectors-section.component";
import { SettingsAccountSectionComponent } from "./sections/settings-account-section.component";
import { SettingsPrivacySectionComponent } from "./sections/settings-privacy-section.component";
import { SettingsObsidianSectionComponent } from "./sections/settings-obsidian-section.component";
import { SettingsAboutSectionComponent } from "./sections/settings-about-section.component";

/** One entry in the macOS-style Settings sidebar. `keywords` feeds the search box. */
interface SettingsSection {
  readonly id: string;
  readonly label: string;
  readonly keywords: string;
}

/**
 * The sidebar sections, in display order. `keywords` are matched (alongside the
 * label) by the search box so typing a setting's name surfaces its section.
 */
const SETTINGS_SECTIONS: readonly SettingsSection[] = [
  { id: "appearance", label: "Appearance", keywords: "theme light dark system look colour color mode" },
  { id: "general", label: "General", keywords: "vault folder subfolder whisper model path setup onboarding" },
  { id: "transcription", label: "Transcription", keywords: "language quality whisper model download on-device size accuracy" },
  { id: "audio", label: "Audio & Capture", keywords: "microphone input device system audio vad smart speech detection high fidelity masters diarization remote speakers echo cancellation aec voice trigger hands-free" },
  { id: "storage", label: "Storage", keywords: "disk space usage recordings audio size limit cap gb delete old cleanup prune free up finder location" },
  { id: "notes", label: "Notes", keywords: "summary style brief detailed action language auto organize subfolders thematic enhance skeleton typed notes append" },
  // Stage-2 hub: Brain & AI + Providers collapsed into ONE section (keywords merged).
  { id: "ai", label: "AI & Models", keywords: "provider assistant backend cloud local gguf model reasoning effort semantic search embedding reindex in-meeting voice assistant wake anthropic ollama claude code gateway openai api key availability binary default consent revoke privacy egress" },
  { id: "connectors", label: "Connectors", keywords: "web search brave egress api key internet" },
  { id: "account", label: "Account", keywords: "sign in login sign up account sharing share link server e2ee zero knowledge recovery" },
  { id: "privacy", label: "Privacy & Integrations", keywords: "redaction firewall cloud processing consent locked folders mcp server claude desktop memory remember facts user memory cross-meeting forget" },
  { id: "obsidian", label: "Obsidian", keywords: "vault markdown notes companion export wikilinks" },
  { id: "about", label: "About", keywords: "about version update check for updates release changelog product info" },
];

/**
 * Settings SHELL (Stage-1 split): owns the macOS-style sidebar (search +
 * section nav + Save), section switching, and kicking off the config
 * load/save orchestration. All form + cross-section state lives in
 * SettingsStore — PROVIDED HERE so the store's lifetime is exactly this
 * route's (behavior-identical to the pre-split monolith). Each section's
 * controls render in a child component under ./sections.
 */
@Component({
  selector: "app-settings",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  // Esc in settings backs out ("← Murmur") — but NOT while you're typing: in the
  // search box Esc clears/blurs it first, and it never hijacks another form field.
  // Declarative host listener — Angular owns its lifecycle (no manual DOM listener).
  host: { "(document:keydown.escape)": "onEscape()" },
  providers: [SettingsStore],
  imports: [
    ReactiveFormsModule,
    SettingsAppearanceSectionComponent,
    SettingsGeneralSectionComponent,
    SettingsTranscriptionSectionComponent,
    SettingsAudioSectionComponent,
    SettingsStorageSectionComponent,
    SettingsNotesSectionComponent,
    SettingsAiSectionComponent,
    SettingsConnectorsSectionComponent,
    SettingsAccountSectionComponent,
    SettingsPrivacySectionComponent,
    SettingsObsidianSectionComponent,
    SettingsAboutSectionComponent,
  ],
  template: `
    <section class="settings-shell">
      <!-- macOS-style left rail: Back, search over sections, then the section list. -->
      <aside class="settings-sidebar drill-rail" aria-label="Settings">
        <!-- Drag strip mirrors the primary rail so the overlay traffic lights
             stay clear of the Back button when the rail is flush to the edge. -->
        <div class="rail-drag" data-tauri-drag-region></div>

        <!-- Drill-down "up": returns to the last non-settings route (or /record). -->
        <button
          type="button"
          class="rail-back"
          (click)="nav.back()"
          aria-label="Back to Murmur"
        >
          <svg
            class="rail-back-icon"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M9.5 3.5 5 8l4.5 4.5" />
          </svg>
          <span class="rail-back-label">Murmur</span>
        </button>

        <div class="sidebar-search">
          <svg
            class="sidebar-search-icon"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            aria-hidden="true"
          >
            <circle cx="7" cy="7" r="4.5" />
            <path d="M10.5 10.5 14 14" />
          </svg>
          <input
            type="search"
            class="sidebar-search-input"
            [formControl]="searchControl"
            placeholder="Search"
            aria-label="Search settings"
            autocomplete="off"
            spellcheck="false"
          />
        </div>

        <nav class="sidebar-nav" aria-label="Settings sections">
          @for (s of visibleSections(); track s.id) {
            <button
              type="button"
              class="nav-item"
              [class.active]="activeSection() === s.id"
              [attr.aria-current]="activeSection() === s.id ? 'page' : null"
              (click)="selectSection(s.id)"
            >
              <span class="nav-icon" aria-hidden="true">
                @switch (s.id) {
                  @case ("appearance") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="8" r="2.6" /><path d="M8 1.5v1.6M8 12.9v1.6M2.4 2.4l1.1 1.1M12.5 12.5l1.1 1.1M1.5 8h1.6M12.9 8h1.6M2.4 13.6l1.1-1.1M12.5 3.5l1.1-1.1" /></svg>
                  }
                  @case ("general") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M2 4.5h8M13 4.5h1M2 11.5h5M10 11.5h4" /><circle cx="11" cy="4.5" r="1.6" /><circle cx="8" cy="11.5" r="1.6" /></svg>
                  }
                  @case ("transcription") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M2 8h1.5M5 5v6M8 2.5v11M11 5v6M14 8h-1.5" /></svg>
                  }
                  @case ("audio") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="6" y="1.5" width="4" height="8" rx="2" /><path d="M3.5 7.5a4.5 4.5 0 0 0 9 0M8 12v2.5" /></svg>
                  }
                  @case ("storage") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2.5" y="3" width="11" height="10" rx="1.6" /><path d="M2.5 6.5h11M5 9.5h.01M5 11.3h3" /></svg>
                  }
                  @case ("notes") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 1.5h5l3 3v10H4z" /><path d="M9 1.5v3h3M5.8 8h4.4M5.8 10.6h4.4" /></svg>
                  }
                  @case ("ai") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M8 2.2 9 5l2.8 1L9 7l-1 2.8L7 7 4.2 6 7 5z" /><path d="M12 9.5l.6 1.5 1.5.6-1.5.6-.6 1.5-.6-1.5L9.4 11.6l1.5-.6z" /></svg>
                  }
                  @case ("connectors") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="8" r="6.2" /><path d="M1.8 8h12.4M8 1.8c1.8 1.7 2.8 3.9 2.8 6.2S9.8 12.5 8 14.2C6.2 12.5 5.2 10.3 5.2 8S6.2 3.5 8 1.8z" /></svg>
                  }
                  @case ("account") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="5.5" r="2.6" /><path d="M3 13.4c.9-2.4 2.7-3.6 5-3.6s4.1 1.2 5 3.6" /></svg>
                  }
                  @case ("privacy") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M8 1.6 3 3.5v3.4c0 3 2 5.3 5 6.1 3-.8 5-3.1 5-6.1V3.5L8 1.6z" /><path d="M6 7.7 7.4 9.1 10 6.3" /></svg>
                  }
                  @case ("obsidian") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M6 1.8 2.8 5.3 4.7 12 7 14l-.8-6z" /><path d="M6 1.8 6.2 8 7 14l3.4-2.6L12 5.4 9 2z" /></svg>
                  }
                  @case ("about") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="8" r="6.2" /><path d="M8 7.4v3.6M8 5.1h.01" /></svg>
                  }
                }
              </span>
              <span class="nav-label">{{ s.label }}</span>
            </button>
          } @empty {
            <p class="nav-empty text-muted">
              No settings match “{{ searchQuery() }}”.
            </p>
          }
        </nav>

        <!-- Auto-save: every change persists on its own (no Save button);
             the pill is the passive "all changes saved" confirmation. -->
        @if (saved()) {
          <div class="sidebar-footer">
            <span class="pill is-success saved-pill">
              <span class="pill-dot"></span>
              Saved
            </span>
          </div>
        }
      </aside>

      <!-- Right pane: the selected section's controls. -->
      <div class="settings-content">
        @if (loadError(); as err) {
          <div class="banner is-danger" role="alert">
            <span class="banner-icon" aria-hidden="true">!</span>
            <span>Couldn't load settings: {{ err }}</span>
          </div>
        }

        <header class="content-header">
          <h2>{{ activeSectionLabel() }}</h2>
        </header>

        <div class="section-body">
          @switch (activeSection()) {
            @case ("appearance") {
              <app-settings-appearance-section />
            }
            @case ("general") {
              <app-settings-general-section />
            }
            @case ("transcription") {
              <app-settings-transcription-section />
            }
            @case ("audio") {
              <app-settings-audio-section />
            }
            @case ("storage") {
              <app-settings-storage-section />
            }
            @case ("notes") {
              <app-settings-notes-section />
            }
            @case ("ai") {
              <app-settings-ai-section />
            }
            @case ("connectors") {
              <app-settings-connectors-section />
            }
            @case ("account") {
              <app-settings-account-section />
            }
            @case ("privacy") {
              <app-settings-privacy-section />
            }
            @case ("obsidian") {
              <app-settings-obsidian-section />
            }
            @case ("about") {
              <app-settings-about-section />
            }
          }
        </div>
      </div>
    </section>
  `,
  styles: [
    `
      /* Settings is a full drill-down (L2): the primary app rail is hidden
         (app-shell) and this host fills the whole window as a flush-left
         [section rail | content] layout. Fixed so it ignores .app-main's
         centered max-width + padding and hugs the viewport edges; below the
         toast viewport (z-index 60). */
      :host {
        position: fixed;
        inset: 0;
        z-index: 5;
        display: block;
        background: var(--surface-base);
      }

      /* ── Two-pane shell: floating section rail + scrolling content ── */
      .settings-shell {
        display: grid;
        grid-template-columns: 246px minmax(0, 1fr);
        height: 100vh;
        height: 100dvh;
      }

      /* Left rail — the shared floating liquid-glass panel (global .drill-rail
         in styles.css carries the panel look; only layout + the enter glide
         live here). */
      .settings-sidebar {
        gap: var(--space-3);
        animation: settings-enter 300ms cubic-bezier(0.22, 1, 0.36, 1) both;
      }

      /* Enter transition — the rail and the content pane share ONE cohesive,
         smoothly-eased glide (content lags 40ms for a touch of depth). It is
         TRANSFORM ONLY, never opacity: the position:fixed :host is opaque and
         near-black (--surface-base), so any opacity fade shows through as a
         "black background, then the UI jumps in" flash (the reported bug). Staying
         opaque means the settings surface is painted from the very first frame and
         simply settles into place. Disabled under reduced-motion below. */
      @keyframes settings-enter {
        from {
          transform: translateY(8px);
        }
        to {
          transform: none;
        }
      }

      /* Top drag strip — mirrors the primary rail so the overlay traffic lights
         have somewhere to float and the window stays draggable up here. */
      .rail-drag {
        flex: none;
        height: 30px;
      }

      /* "← Murmur" drill-down up-affordance. */
      .rail-back {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        width: 100%;
        height: 34px;
        padding: 0 var(--space-2);
        border: 0;
        border-radius: var(--radius-md);
        background: transparent;
        color: var(--text-secondary);
        font: inherit;
        font-size: 0.9rem;
        font-weight: 600;
        letter-spacing: -0.01em;
        text-align: left;
        cursor: pointer;
        transition:
          background var(--transition-fast),
          color var(--transition-fast);
      }
      .rail-back:hover {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      .rail-back:focus-visible {
        outline: none;
        color: var(--text-primary);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .rail-back-icon {
        flex: none;
        width: 16px;
        height: 16px;
      }

      .sidebar-search {
        position: relative;
        display: flex;
        align-items: center;
      }
      .sidebar-search-icon {
        position: absolute;
        left: var(--space-3);
        width: 15px;
        height: 15px;
        color: var(--text-muted);
        pointer-events: none;
      }
      .sidebar-search-input {
        width: 100%;
        height: 34px;
        padding: 0 var(--space-3) 0 calc(var(--space-6) + var(--space-1));
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font: inherit;
        font-size: 0.875rem;
      }
      .sidebar-search-input::placeholder {
        color: var(--text-muted);
      }
      .sidebar-search-input:focus-visible {
        outline: none;
        border-color: var(--accent-hover);
        box-shadow: 0 0 0 3px var(--accent-soft);
      }
      /* Hide the native WebKit search clear affordance for a clean rail. */
      .sidebar-search-input::-webkit-search-decoration,
      .sidebar-search-input::-webkit-search-cancel-button {
        -webkit-appearance: none;
      }

      .sidebar-nav {
        display: flex;
        flex-direction: column;
        gap: 2px;
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
      }
      .nav-item {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        width: 100%;
        padding: var(--space-2) var(--space-3);
        border: 0;
        border-radius: var(--radius-md);
        background: transparent;
        color: var(--text-secondary);
        font: inherit;
        font-size: 0.9rem;
        font-weight: 550;
        text-align: left;
        cursor: pointer;
        transition:
          background var(--transition-fast),
          color var(--transition-fast);
      }
      .nav-item:hover {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      /* Active: the shell's neutral glass-on-glass pill — accent only on the
         glyph/label, never as the fill (matches the primary rail). */
      .nav-item.active {
        background: var(--shell-active-bg);
        color: var(--shell-active-text);
        box-shadow: var(--shell-active-shadow);
      }
      .nav-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 20px;
        height: 20px;
        flex: none;
        color: currentColor;
      }
      .nav-icon svg {
        width: 17px;
        height: 17px;
      }
      .nav-label {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .nav-empty {
        margin: var(--space-2) var(--space-1);
        font-size: 0.85rem;
        line-height: 1.5;
      }

      .sidebar-footer {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex: none;
        flex-wrap: wrap;
        margin-top: var(--space-1);
        padding-top: var(--space-3);
        border-top: 1px solid var(--border-subtle);
      }
      .saved-pill {
        flex: none;
      }

      /* Right pane — scrolls independently of the fixed rail. It provides its own
         padding (the shell no longer sits inside .app-main's padded column). */
      .settings-content {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        min-width: 0;
        height: 100%;
        overflow-y: auto;
        padding: var(--space-7) var(--space-6) var(--space-8);
      }
      /* Cap the reading width AND center the column — the same centered
         max-width reading column as the main pages (.app-main). The error
         banner is a direct .settings-content child too, so it rides the same
         rule (it otherwise spans the full pane width). */
      .content-header,
      .section-body,
      .settings-content > .banner {
        width: 100%;
        max-width: var(--content-max);
        margin: 0 auto;
      }
      .content-header h2 {
        margin: 0;
        font-size: 1.35rem;
        font-weight: 650;
        letter-spacing: -0.01em;
      }
      .section-body {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        /* Shares the rail's cohesive transform-only glide (NOT the global 'rise',
           which fades opacity 0→1 and would flash near-black over the :host — on
           entry AND on every section switch). 40ms lag for a subtle stagger. */
        animation: settings-enter 300ms cubic-bezier(0.22, 1, 0.36, 1) 40ms both;
      }

      /* Narrow widths: stack the rail on top of the content (rows), each
         scrolling within the fixed full-height shell. */
      @media (max-width: 640px) {
        .settings-shell {
          grid-template-columns: 1fr;
          grid-template-rows: auto minmax(0, 1fr);
        }
        .settings-sidebar {
          height: auto;
          margin: 10px 10px 0;
        }
        .sidebar-nav {
          flex-direction: row;
          flex-wrap: wrap;
          overflow-y: visible;
        }
        .nav-item {
          width: auto;
        }
        .settings-content {
          padding: var(--space-5) var(--space-4) var(--space-6);
        }
      }

      /* Honor reduced-motion: no rail slide, no section rise. */
      @media (prefers-reduced-motion: reduce) {
        .settings-sidebar,
        .section-body {
          animation: none;
        }
      }

      /* --- Banner icon (matches the record screen) --- */
      .banner-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 24px;
        height: 24px;
        min-width: 24px;
        border-radius: 50%;
        background: rgba(255, 255, 255, 0.08);
        font-weight: 700;
        font-size: 0.85rem;
        line-height: 1;
      }
    `,
  ],
})
export class SettingsComponent implements OnInit {
  /** Shared settings state — provided on this component (see class JSDoc). */
  private readonly store = inject(SettingsStore);

  /** Drill-down back navigation ("← Murmur" + Esc) — no settings state coupling. */
  readonly nav = inject(NavHistoryService);

  /**
   * Esc while in settings. Backs out to where you came from — EXCEPT while you're
   * typing: in the search box the first Esc clears it (or blurs when empty), and
   * Esc is ignored inside any other form field, so it never ejects you mid-edit.
   */
  onEscape(): void {
    const el = document.activeElement as HTMLElement | null;
    if (el?.classList.contains("sidebar-search-input")) {
      if (this.searchControl.value) {
        this.searchControl.setValue("");
      } else {
        el.blur();
      }
      return;
    }
    const tag = el?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
      return;
    }
    this.nav.back();
  }

  // ── macOS-style navigation: sidebar sections + search ───────────────────

  /** The sidebar sections (icons rendered in the template by id). */
  readonly sections = SETTINGS_SECTIONS;

  /** The currently-shown section (right pane). Defaults to Appearance. */
  readonly activeSection = signal<string>(SETTINGS_SECTIONS[0].id);

  /** Search box for filtering the sidebar section list. Not part of the config form. */
  readonly searchControl = new FormControl("", { nonNullable: true });

  /** Live signal of the (raw) search text, seeded so `computed`s track it. */
  private readonly _search = toSignal(
    this.searchControl.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );

  /** Trimmed query — shown in the "no results" message. */
  readonly searchQuery = computed(() => this._search().trim());

  /**
   * Sections that match the search query (by label + keywords). With no query,
   * every section is shown. Filtering the sidebar only — the visible content
   * pane is driven by `activeSection` and is never changed by a search.
   */
  readonly visibleSections = computed(() => {
    const q = this.searchQuery().toLowerCase();
    if (!q) return this.sections;
    return this.sections.filter((s) =>
      (s.label + " " + s.keywords).toLowerCase().includes(q),
    );
  });

  /** Human label for the active section (right-pane header). */
  readonly activeSectionLabel = computed(
    () =>
      this.sections.find((s) => s.id === this.activeSection())?.label ?? "",
  );

  /** Switch the visible section (sidebar click). */
  selectSection(id: string): void {
    this.activeSection.set(id);
  }

  /** Save/load state surfaced from the store — the sidebar template reads these. */
  readonly saved = this.store.saved;
  readonly loadError = this.store.loadError;

  ngOnInit(): void {
    // Pre-split this was an async ngOnInit whose promise Angular ignored;
    // `void` keeps identical fire-and-forget semantics.
    void this.store.load();
  }
}
