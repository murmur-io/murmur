import { ChangeDetectionStrategy, Component } from "@angular/core";
import { AiAdvancedBlockComponent } from "./ai/ai-advanced-block.component";
import { AiPrivacyStripComponent } from "./ai/ai-privacy-strip.component";
import { AiResolvedMapComponent } from "./ai/ai-resolved-map.component";
import { BrainPostureBlockComponent } from "./ai/brain-posture-block.component";
import { DuringMeetingsBlockComponent } from "./ai/during-meetings-block.component";
import { OnDeviceIntelligenceBlockComponent } from "./ai/on-device-intelligence-block.component";

/**
 * Settings → "AI & Models" section (Stage-2 hub): the former Brain & AI +
 * Providers sections (and General's provider dropdown) collapsed into one
 * surface, per docs/research/2026-07-02-unify-model-settings.md. Five
 * blocks, each its own child under ./ai (keeps every component well under
 * the style budget):
 *   A — Brain posture (Cloud / Hybrid / Fully local preset — Task 2);
 *   B — Advanced disclosure (Task 4): collapsed expander wrapping provider
 *       connection cards + Default AI + per-feature role rows;
 *   C — Live during meetings (Task 5): in-meeting voice assistant +
 *       proactive hints toggles + cloud-egress consent warning;
 *   D — On-device intelligence (Task 5): always-on badges + semantic
 *       search toggle + embedding-model download + re-index controls;
 *   E — Privacy strip (where-your-text-goes consent Allow/Revoke).
 * All state stays in the shell-provided SettingsStore.
 */
@Component({
  selector: "app-settings-ai-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    BrainPostureBlockComponent,
    AiResolvedMapComponent,
    AiAdvancedBlockComponent,
    DuringMeetingsBlockComponent,
    OnDeviceIntelligenceBlockComponent,
    AiPrivacyStripComponent,
  ],
  template: `
    <app-brain-posture-block />
    <app-ai-resolved-map />
    <app-ai-advanced-block />
    <app-during-meetings-block />
    <app-on-device-intelligence-block />
    <app-ai-privacy-strip />
  `,
  styles: [
    `
      /* Layout-transparent (like every section child): each block's .card is
         a direct flex item of the shell's .section-body, keeping the same
         inter-card spacing as the other sections. */
      :host {
        display: contents;
      }
    `,
  ],
})
export class SettingsAiSectionComponent {}
