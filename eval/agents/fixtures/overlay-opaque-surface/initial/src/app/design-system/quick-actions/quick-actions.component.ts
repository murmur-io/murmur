import { ChangeDetectionStrategy, Component, input, output } from '@angular/core';

export interface QuickAction {
  readonly id: string;
  readonly label: string;
}

@Component({
  selector: 'mur-quick-actions',
  templateUrl: './quick-actions.component.html',
  styleUrl: './quick-actions.component.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class QuickActionsComponent {
  readonly actions = input<readonly QuickAction[]>([]);
  readonly chosen = output<string>();
}
