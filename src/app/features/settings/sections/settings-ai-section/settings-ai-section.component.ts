import { ChangeDetectionStrategy, Component } from "@angular/core";
import { AiAdvancedBlockComponent } from "../ai/ai-advanced-block/ai-advanced-block.component";
import { AiPrivacyStripComponent } from "../ai/ai-privacy-strip/ai-privacy-strip.component";
import { AiResolvedMapComponent } from "../ai/ai-resolved-map/ai-resolved-map.component";
import { AiSetupBlockComponent } from "../ai/ai-setup-block/ai-setup-block.component";
import { BrainPostureBlockComponent } from "../ai/brain-posture-block/brain-posture-block.component";
import { DuringMeetingsBlockComponent } from "../ai/during-meetings-block/during-meetings-block.component";
import { OnDeviceIntelligenceBlockComponent } from "../ai/on-device-intelligence-block/on-device-intelligence-block.component";

/**
 * Settings → "AI & Models" section (Stage-2 hub): the former Brain & AI +
 * Providers sections (and General's provider dropdown) collapsed into one
 * surface, per docs/research/2026-07-02-unify-model-settings.md and the
 * posture-driven redesign (docs/superpowers/specs/2026-07-05-…). Blocks,
 * each its own child under ./ai (keeps every component well under the style
 * budget):
 *   A — Where your AI runs (Cloud / Hybrid / Fully local posture hero);
 *   B — Your setup (posture-adaptive): the Default AI engine and/or the
 *       on-device model pickers for the chosen lane;
 *   C — What runs where: the resolved map, grouped cloud vs on-Mac;
 *   D — Advanced disclosure: connection cards + per-feature role rows;
 *   E — Live during meetings: in-meeting voice assistant + proactive hints;
 *   F — On-device intelligence: always-on badges + semantic search + models;
 *   G — Privacy strip (where-your-text-goes consent Allow/Revoke).
 * All state stays in the shell-provided SettingsStore.
 */
@Component({
  selector: "app-settings-ai-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    BrainPostureBlockComponent,
    AiSetupBlockComponent,
    AiResolvedMapComponent,
    AiAdvancedBlockComponent,
    DuringMeetingsBlockComponent,
    OnDeviceIntelligenceBlockComponent,
    AiPrivacyStripComponent,
  ],
  templateUrl: "./settings-ai-section.component.html",
  styleUrl: "./settings-ai-section.component.scss",
})
export class SettingsAiSectionComponent {}
