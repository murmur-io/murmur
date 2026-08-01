import { Component, effect, signal } from '@angular/core';

@Component({ standalone: true, template: '' })
export class ExampleComponent {
  readonly source = signal(0);
  readonly mirrored = signal(0);

  constructor() {
    effect(() => this.mirrored.set(this.source()));
  }
}
