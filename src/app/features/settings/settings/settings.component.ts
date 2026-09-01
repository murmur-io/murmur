import {
  ChangeDetectionStrategy,
  Component,
  type ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { ActivatedRoute } from "@angular/router";
import { startWith } from "rxjs";
import { NavHistoryService } from "../../../core/nav-history.service";
import { MurSidebarComponent } from "../../../design-system/sidebar/sidebar.component";
import { SettingsStore } from "../settings.store";
import { SettingsAppearanceSectionComponent } from "../sections/settings-appearance-section/settings-appearance-section.component";
import { SettingsGeneralSectionComponent } from "../sections/settings-general-section/settings-general-section.component";
import { SettingsTranscriptionSectionComponent } from "../sections/settings-transcription-section/settings-transcription-section.component";
import { SettingsAudioSectionComponent } from "../sections/settings-audio-section/settings-audio-section.component";
import { SettingsStorageSectionComponent } from "../sections/settings-storage-section/settings-storage-section.component";
import { SettingsNotesSectionComponent } from "../sections/settings-notes-section/settings-notes-section.component";
import { SettingsAiSectionComponent } from "../sections/settings-ai-section/settings-ai-section.component";
import { SettingsConnectorsSectionComponent } from "../sections/settings-connectors-section/settings-connectors-section.component";
import { SettingsAccountSectionComponent } from "../sections/settings-account-section/settings-account-section.component";
import { SettingsOrganizationSectionComponent } from "../sections/settings-organization-section/settings-organization-section.component";
import { SettingsPrivacySectionComponent } from "../sections/settings-privacy-section/settings-privacy-section.component";
import { SettingsObsidianSectionComponent } from "../sections/settings-obsidian-section/settings-obsidian-section.component";
import { SettingsImportsSectionComponent } from "../sections/settings-imports-section/settings-imports-section.component";
import { SettingsAboutSectionComponent } from "../sections/settings-about-section/settings-about-section.component";
import { SettingsDeveloperSectionComponent } from "../sections/settings-developer-section/settings-developer-section.component";

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
  { id: "connectors", label: "Connectors", keywords: "web search brave egress api key internet jira slack notion clickup token tasks pages" },
  { id: "account", label: "Account", keywords: "sign in login sign up account sharing share link server e2ee zero knowledge recovery" },
  { id: "organization", label: "Organization", keywords: "org team shared brain company workspace members invite colleague owner consent sync org brain shared knowledge base leave" },
  { id: "privacy", label: "Privacy & Integrations", keywords: "redaction firewall cloud processing consent locked folders mcp server claude desktop memory remember facts user memory cross-meeting forget" },
  { id: "obsidian", label: "Obsidian", keywords: "vault markdown notes companion export wikilinks" },
  { id: "imports", label: "Imports", keywords: "notion obsidian apple notes import migrate export zip markdown vault bring in move from another app switch migration" },
  { id: "developer", label: "Developer", keywords: "developer mode logs log file diagnostics debug crash troubleshooting console tools advanced" },
  { id: "about", label: "About", keywords: "about version update check for updates release changelog product info" },
];

/**
 * Sidebar grouping (mirrors the main rail's labeled clusters): the two
 * everyday sections sit ungrouped on top, then labeled categories, with About
 * ungrouped at the bottom. `key` = the first id (stable @for identity — labels
 * repeat `null`).
 */
const SETTINGS_GROUPS: readonly {
  readonly label: string | null;
  readonly ids: readonly string[];
}[] = [
  { label: null, ids: ["appearance", "general"] },
  { label: "Capture", ids: ["transcription", "audio", "storage"] },
  { label: "Intelligence", ids: ["notes", "ai", "connectors"] },
  { label: "Sharing", ids: ["account", "organization"] },
  { label: "Privacy & Vault", ids: ["privacy", "obsidian", "imports"] },
  { label: null, ids: ["developer", "about"] },
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
  changeDetection: ChangeDetectionStrategy.OnPush,
  providers: [SettingsStore],
  imports: [
    MurSidebarComponent,
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
    SettingsOrganizationSectionComponent,
    SettingsPrivacySectionComponent,
    SettingsObsidianSectionComponent,
    SettingsImportsSectionComponent,
    SettingsAboutSectionComponent,
    SettingsDeveloperSectionComponent,
  ],
  templateUrl: "./settings.component.html",
  styleUrl: "./settings.component.scss",
})
export class SettingsComponent implements OnInit {
  /** Shared settings state — provided on this component (see class JSDoc). */
  private readonly store = inject(SettingsStore);

  /** Drill-down back navigation ("← Murmur" + Esc) — no settings state coupling. */
  readonly nav = inject(NavHistoryService);

  /** Deep-link support: `?section=<id>` (e.g. from the Library "Shared brains" rail's
   * settings shortcut) opens directly on that section instead of the Appearance default. */
  private readonly route = inject(ActivatedRoute);

  private readonly injector = inject(Injector);

  /** The modal panel — focused on open so Esc and Tab start inside the dialog. */
  private readonly panel = viewChild<ElementRef<HTMLElement>>("panel");

  /**
   * Click the backdrop to leave, the standard modal dismissal. Only a click that
   * LANDS on the scrim itself counts — one that bubbles up from a control inside
   * the panel must never close the screen the user is working in.
   */
  onScrimClick(event: MouseEvent): void {
    if (event.target === event.currentTarget) {
      this.nav.back();
    }
  }

  /**
   * Esc while in settings. Backs out to where you came from — EXCEPT while you're
   * typing: in the search box the first Esc clears it (or blurs when empty), and
   * Esc is ignored inside any other form field, so it never ejects you mid-edit.
   *
   * Bound on the SCRIM rather than on `document` now that Settings is a modal:
   * the dialog focuses itself on open, so every Escape reaches it by bubbling,
   * and the handler can no longer fire for a keystroke aimed at something else.
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

  /**
   * The sidebar's grouped view of `visibleSections`: sections in their
   * category, categories whose every section is filtered out disappear.
   */
  readonly visibleGroups = computed(() => {
    const visible = new Set(this.visibleSections().map((s) => s.id));
    const byId = new Map(this.sections.map((s) => [s.id, s]));
    return SETTINGS_GROUPS.map((g) => ({
      label: g.label,
      key: g.ids[0],
      sections: g.ids
        .filter((id) => visible.has(id))
        .map((id) => byId.get(id))
        .filter((s): s is SettingsSection => s !== undefined),
    })).filter((g) => g.sections.length > 0);
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

  constructor() {
    // A dialog should own the keyboard the moment it opens: without this the
    // focus stays on whatever opened Settings, behind the scrim.
    afterNextRender(() => this.panel()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  ngOnInit(): void {
    // Pre-split this was an async ngOnInit whose promise Angular ignored;
    // `void` keeps identical fire-and-forget semantics.
    void this.store.load();
    // `?section=<id>` deep-link — only honored when it names a REAL section, so a stale/typo'd
    // link falls back to the default (Appearance) instead of a blank right pane.
    const requested = this.route.snapshot.queryParamMap.get("section");
    if (requested && this.sections.some((s) => s.id === requested)) {
      this.activeSection.set(requested);
    }
  }
}
