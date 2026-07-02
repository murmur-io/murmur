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
import { SettingsStore } from "./settings.store";
import { SettingsAppearanceSectionComponent } from "./sections/settings-appearance-section.component";
import { SettingsGeneralSectionComponent } from "./sections/settings-general-section.component";
import { SettingsTranscriptionSectionComponent } from "./sections/settings-transcription-section.component";
import { SettingsAudioSectionComponent } from "./sections/settings-audio-section.component";
import { SettingsNotesSectionComponent } from "./sections/settings-notes-section.component";
import { SettingsBrainSectionComponent } from "./sections/settings-brain-section.component";
import { SettingsConnectorsSectionComponent } from "./sections/settings-connectors-section.component";
import { SettingsProvidersSectionComponent } from "./sections/settings-providers-section.component";
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
  { id: "general", label: "General", keywords: "provider vault folder subfolder whisper model path setup onboarding" },
  { id: "transcription", label: "Transcription", keywords: "language quality whisper model download on-device size accuracy" },
  { id: "audio", label: "Audio & Capture", keywords: "microphone input device system audio vad smart speech detection high fidelity masters diarization remote speakers echo cancellation aec voice trigger hands-free" },
  { id: "notes", label: "Notes", keywords: "summary style brief detailed action language auto organize subfolders thematic" },
  { id: "brain", label: "Brain & AI", keywords: "assistant backend cloud local gguf model reasoning effort semantic search embedding reindex in-meeting voice assistant wake" },
  { id: "connectors", label: "Connectors", keywords: "web search brave egress api key internet" },
  { id: "providers", label: "Providers", keywords: "anthropic ollama claude code gateway openai api key availability model binary" },
  { id: "privacy", label: "Privacy & Integrations", keywords: "redaction firewall cloud processing consent locked folders mcp server claude desktop" },
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
  providers: [SettingsStore],
  imports: [
    ReactiveFormsModule,
    SettingsAppearanceSectionComponent,
    SettingsGeneralSectionComponent,
    SettingsTranscriptionSectionComponent,
    SettingsAudioSectionComponent,
    SettingsNotesSectionComponent,
    SettingsBrainSectionComponent,
    SettingsConnectorsSectionComponent,
    SettingsProvidersSectionComponent,
    SettingsPrivacySectionComponent,
    SettingsObsidianSectionComponent,
    SettingsAboutSectionComponent,
  ],
  template: `
    <section class="settings-shell">
      <!-- macOS-style left rail: search over sections, then the section list. -->
      <aside class="settings-sidebar" aria-label="Settings">
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
                  @case ("notes") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M4 1.5h5l3 3v10H4z" /><path d="M9 1.5v3h3M5.8 8h4.4M5.8 10.6h4.4" /></svg>
                  }
                  @case ("brain") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M8 2.2 9 5l2.8 1L9 7l-1 2.8L7 7 4.2 6 7 5z" /><path d="M12 9.5l.6 1.5 1.5.6-1.5.6-.6 1.5-.6-1.5L9.4 11.6l1.5-.6z" /></svg>
                  }
                  @case ("connectors") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="8" r="6.2" /><path d="M1.8 8h12.4M8 1.8c1.8 1.7 2.8 3.9 2.8 6.2S9.8 12.5 8 14.2C6.2 12.5 5.2 10.3 5.2 8S6.2 3.5 8 1.8z" /></svg>
                  }
                  @case ("providers") {
                    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="2.5" width="12" height="4.5" rx="1.4" /><rect x="2" y="9" width="12" height="4.5" rx="1.4" /><path d="M4.4 4.75h.01M4.4 11.25h.01" /></svg>
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

        <!-- Save applies the whole form regardless of the visible section. -->
        <div class="sidebar-footer">
          <button type="button" class="btn btn-primary sidebar-save" (click)="save()">
            Save settings
          </button>
          @if (saved()) {
            <span class="pill is-success saved-pill">
              <span class="pill-dot"></span>
              Saved
            </span>
          }
        </div>
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
            @case ("notes") {
              <app-settings-notes-section />
            }
            @case ("brain") {
              <app-settings-brain-section />
            }
            @case ("connectors") {
              <app-settings-connectors-section />
            }
            @case ("providers") {
              <app-settings-providers-section />
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
      /* ── macOS-style two-pane shell: sidebar + content ── */
      .settings-shell {
        display: grid;
        grid-template-columns: 216px minmax(0, 1fr);
        gap: var(--space-5);
        align-items: start;
      }

      /* Left rail — sticky under the app header, its own quiet frosted panel. */
      .settings-sidebar {
        position: sticky;
        top: calc(var(--space-8) + var(--space-2));
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-3);
        border-radius: var(--radius-lg);
        background: var(--surface-raised);
        border: 1px solid var(--glass-border);
        box-shadow: var(--glass-highlight);
        animation: rise 380ms var(--transition) both;
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
        background: var(--surface-input);
        color: var(--text-primary);
      }
      .nav-item.active {
        background: var(--accent-soft);
        color: var(--accent);
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
        flex-wrap: wrap;
        margin-top: var(--space-1);
        padding-top: var(--space-3);
        border-top: 1px solid var(--border-subtle);
      }
      .sidebar-save {
        flex: 1 1 auto;
      }
      .saved-pill {
        flex: none;
      }

      /* Right pane — the section title + its stacked cards. */
      .settings-content {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        min-width: 0;
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
        animation: rise 320ms var(--transition) both;
      }

      /* Collapse to a single column on narrow widths (sidebar stacks on top). */
      @media (max-width: 640px) {
        .settings-shell {
          grid-template-columns: 1fr;
        }
        .settings-sidebar {
          position: static;
        }
        .sidebar-nav {
          flex-direction: row;
          flex-wrap: wrap;
        }
        .nav-item {
          width: auto;
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

  /** Save applies the whole form regardless of the visible section. */
  save(): void {
    void this.store.save();
  }
}
