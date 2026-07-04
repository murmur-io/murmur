import { ChangeDetectionStrategy, Component } from "@angular/core";
import { AiConnectionCardsComponent } from "./ai/ai-connection-cards.component";
import { AiDefaultsBlockComponent } from "./ai/ai-defaults-block.component";
import { AiPrivacyStripComponent } from "./ai/ai-privacy-strip.component";
import { BrainPostureBlockComponent } from "./ai/brain-posture-block.component";

/**
 * Settings → "AI & Models" section (Stage-2 hub): the former Brain & AI +
 * Providers sections (and General's provider dropdown) collapsed into one
 * surface, per docs/research/2026-07-02-unify-model-settings.md. Four
 * blocks, each its own child under ./ai (keeps every component well under
 * the style budget): A — provider connection cards (Local vs Cloud split);
 * B — Murmur Brain posture (Cloud / Hybrid / Fully local preset + retirement
 * nudge + contextual state — Task 2, extracted from the old defaults block);
 * C — what Murmur uses (Default AI + model + role rows + Live + On-device);
 * D — the where-your-text-goes privacy strip with consent Allow/Revoke.
 * All state stays in the shell-provided SettingsStore.
 */
@Component({
  selector: "app-settings-ai-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    AiConnectionCardsComponent,
    BrainPostureBlockComponent,
    AiDefaultsBlockComponent,
    AiPrivacyStripComponent,
  ],
  template: `
    <app-ai-connection-cards />
    <app-brain-posture-block />
    <app-ai-defaults-block />
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
