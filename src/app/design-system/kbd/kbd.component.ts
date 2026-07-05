import { ChangeDetectionStrategy, Component } from "@angular/core";

/** Design System — <mur-kbd>: a keyboard-shortcut chip (⌘K, esc…). */
@Component({
  selector: "mur-kbd",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./kbd.component.html",
  styleUrl: "./kbd.component.scss",
})
export class MurKbdComponent {}
