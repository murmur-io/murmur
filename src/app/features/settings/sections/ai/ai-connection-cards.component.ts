import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { SettingsStore } from "../../settings.store";
import {
  AiConnectionCardComponent,
  type ConnectionCardVm,
} from "./ai-connection-card.component";
import { BrainEngineCardComponent } from "./brain-engine-card.component";

/** One selectable connection (implicit singleton — no named registry yet). */
interface ConnectionDef {
  readonly id: string;
  readonly name: string;
}

/**
 * The four connections, in display order. ALWAYS all rendered — the gateway
 * card no longer hides behind `providerId === 'gateway'` (the old T1
 * split-brain); `provider_statuses` omitting an unconfigured gateway just
 * renders as "Not set up".
 */
const CONNECTIONS: readonly ConnectionDef[] = [
  { id: "claude_code", name: "Claude Code" },
  { id: "anthropic", name: "Anthropic API" },
  { id: "ollama", name: "Ollama" },
  { id: "gateway", name: "Kong AI Gateway" },
];

/**
 * AI & Models → Block A: the provider CONNECTION CARDS, split into
 * "On this Mac" vs "Cloud (redacted first)" groups. Ollama moves between the
 * groups live off the store's per-connection classification
 * (`ollamaIsRemote` — the same source as `providerIsCloud`, mirroring the
 * backend's `egress_is_cloud`). Card rendering lives in
 * AiConnectionCardComponent; this component owns the grouping, the
 * disclosure state, and the Test orchestration.
 */
@Component({
  selector: "app-ai-connection-cards",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [AiConnectionCardComponent, BrainEngineCardComponent],
  template: `
    <div class="card conn-block">
      <div class="conn-head">
        <h3>Engines</h3>
        <p class="text-secondary conn-sub">
          Where models can run. Set each engine up once — pick which one Murmur
          uses under Default engine below.
        </p>
      </div>

      <div class="conn-group">
        <span class="conn-group-label text-muted">On this Mac</span>
        <app-brain-engine-card />
        @for (c of localCards(); track c.id) {
          <app-ai-connection-card
            [card]="c"
            [testing]="testing()"
            (toggleConfigure)="toggleConfigure(c.id)"
            (probe)="test(c.id)"
          />
        } @empty {
          <p class="conn-empty text-muted">
            Ollama appears here only while its base URL points at this Mac.
          </p>
        }
      </div>

      <div class="conn-group">
        <span class="conn-group-label text-muted">Cloud (redacted first)</span>
        @for (c of cloudCards(); track c.id) {
          <app-ai-connection-card
            [card]="c"
            [testing]="testing()"
            (toggleConfigure)="toggleConfigure(c.id)"
            (probe)="test(c.id)"
          />
        }
      </div>
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }

      .conn-block {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .conn-head {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .conn-head h3 {
        margin: 0;
      }
      .conn-sub {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.55;
      }

      .conn-group {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .conn-group-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .conn-empty {
        margin: 0;
        font-size: 0.85rem;
        line-height: 1.5;
      }
    `,
  ],
})
export class AiConnectionCardsComponent {
  private readonly store = inject(SettingsStore);

  /** Which cards have their Configure disclosure open (default: none). */
  private readonly _expanded = signal<ReadonlySet<string>>(new Set());

  /** True while a Test probe is in flight — disables every Test button. */
  readonly testing = signal(false);

  /** Cards for the "On this Mac" group — Ollama, while its URL is loopback. */
  readonly localCards = computed(() => this.buildCards(false));

  /** Cards for the "Cloud (redacted first)" group — everything else. */
  readonly cloudCards = computed(() => this.buildCards(true));

  private buildCards(cloud: boolean): ConnectionCardVm[] {
    const statuses = this.store.providers();
    const expanded = this._expanded();
    const ollamaRemote = this.store.ollamaIsRemote();
    return CONNECTIONS.filter((c) =>
      c.id === "ollama" ? ollamaRemote === cloud : cloud,
    ).map((c) => ({
      ...c,
      status: statuses.find((p) => p.id === c.id) ?? null,
      expanded: expanded.has(c.id),
      cloud,
    }));
  }

  toggleConfigure(id: string): void {
    const next = new Set(this._expanded());
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    this._expanded.set(next);
  }

  /**
   * Test = re-probe availability (the existing provider fan-out; no new IPC).
   * For the gateway it additionally runs the real health probe and opens the
   * disclosure so the result is visible.
   */
  async test(id: string): Promise<void> {
    this.testing.set(true);
    try {
      if (id === "gateway") {
        if (!this._expanded().has("gateway")) this.toggleConfigure("gateway");
        await this.store.checkGatewayHealth();
      }
      await this.store.refreshProviders();
    } finally {
      this.testing.set(false);
    }
  }
}
