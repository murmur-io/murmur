import { ChangeDetectionStrategy, Component, ElementRef, Injector, afterNextRender, computed, inject, viewChild } from "@angular/core";

import type { SmartOrganizeItemKind, SmartOrganizeRule } from "../../../core/models";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { MurPillComponent } from "../../../design-system/pill/pill.component";
import { TeleportToBodyDirective } from "../../../design-system/teleport-to-body.directive";
import { SmartOrganizeService } from "./smart-organize.service";

@Component({
  selector: "app-smart-organize",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent, MurPillComponent, TeleportToBodyDirective],
  templateUrl: "./smart-organize.component.html",
  styleUrl: "./smart-organize.component.scss",
})
export class SmartOrganizeComponent {
  readonly smart = inject(SmartOrganizeService);
  private readonly injector = inject(Injector);
  private readonly closeButton = viewChild<ElementRef<HTMLButtonElement>>("closeButton");
  readonly bucketViews = computed(() => {
    const excluded = this.smart.excluded();
    return (this.smart.plan()?.buckets ?? []).map((bucket) => {
      const itemIds = bucket.items.map((item) => item.itemId);
      const selected = itemIds.filter((id) => !excluded.has(id)).length;
      return {
        bucket,
        itemIds,
        checked: selected === itemIds.length,
        indeterminate: selected > 0 && selected < itemIds.length,
      };
    });
  });

  constructor() {
    afterNextRender(() => this.closeButton()?.nativeElement.focus({ preventScroll: true }), { injector: this.injector });
  }

  onKind(kind: SmartOrganizeItemKind, event: Event): void {
    this.smart.setKind(kind, (event.target as HTMLInputElement).checked);
  }
  onDepth(event: Event): void {
    this.smart.setDepth((event.target as HTMLInputElement).value === "descendants");
  }
  onRule(rule: SmartOrganizeRule): void { this.smart.setRule(rule); }
  onDialogKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape" && !this.smart.applying()) {
      event.preventDefault();
      this.smart.close();
      return;
    }
    if (event.key === "Tab") {
      const dialog = (event.currentTarget as HTMLElement);
      const controls = [...dialog.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), [tabindex="0"]')];
      if (controls.length === 0) {
        event.preventDefault();
        dialog.focus();
        return;
      }
      const first = controls[0];
      const last = controls.at(-1);
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last?.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first?.focus();
      }
    }
  }
}
