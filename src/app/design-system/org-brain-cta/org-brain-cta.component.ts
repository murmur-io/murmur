import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from "@angular/core";

/**
 * Design System — `<mur-org-brain-cta>`: the prominent "Add to Org Brain" call
 * to action shown at the TOP of the note / meeting share surfaces. Org sharing
 * is the flagship action, so this is an accent-tinted card (not a quiet
 * `.panel-card` with a ghost link) with the org mark, a one-line E2EE promise,
 * and a primary button. Purely presentational — it emits `add` and the host
 * panel opens the real `<app-org-share-sheet>`. Shared by `note-share-panel`
 * and the meeting `share-panel` so the two stay identical.
 */
@Component({
  selector: "mur-org-brain-cta",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./org-brain-cta.component.html",
  styleUrl: "./org-brain-cta.component.scss",
})
export class MurOrgBrainCtaComponent {
  /** The target org's display name. */
  readonly orgName = input.required<string>();
  /** Teammate count (drives the "N teammate(s)" copy). */
  readonly memberCount = input.required<number>();
  /** Disables the button (e.g. while the note is mid-edit). */
  readonly disabled = input(false);

  /** Fired when the user clicks the CTA — the host opens the org-share sheet. */
  readonly add = output<void>();

  /** "your N teammate" / "N teammates" — never a bare count. */
  readonly audience = computed(() => {
    const n = this.memberCount();
    return `${n} ${n === 1 ? "teammate" : "teammates"}`;
  });
}
