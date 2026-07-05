import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { BRAIN_ENGINE_ID, SettingsStore } from "../../settings.store";
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
          Every engine a model can run on. Your posture above already picks which
          one runs — “Ready” just means an engine is set up and available, not
          that it’s running. Open this only to route a feature by hand or connect
          your own engine.
        </p>
      </div>

      @if (brainShown() || macCardsShown().length > 0) {
        <div class="conn-group">
          <span class="conn-group-label text-muted">On this Mac</span>
          @if (brainShown()) {
            <app-brain-engine-card [inUse]="brainInUse()" />
          }
          @for (c of macCardsShown(); track c.id) {
            <app-ai-connection-card
              [card]="c"
              [testing]="testing()"
              (toggleConfigure)="toggleConfigure(c.id)"
              (probe)="test(c.id)"
            />
          }
        </div>
      }

      @if (cloudCardsShown().length > 0) {
        <div class="conn-group">
          <span class="conn-group-label text-muted">Cloud (redacted first)</span>
          @for (c of cloudCardsShown(); track c.id) {
            <app-ai-connection-card
              [card]="c"
              [testing]="testing()"
              (toggleConfigure)="toggleConfigure(c.id)"
              (probe)="test(c.id)"
            />
          }
        </div>
      }

      @if (hiddenCount() > 0) {
        <button type="button" class="conn-showall" (click)="toggleShowAll()">
          {{
            showAll()
              ? "Show fewer engines"
              : "Show all engines (" + hiddenCount() + " more)"
          }}
        </button>
      }
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
      /* Quiet text button that reveals the engines outside the posture's lane. */
      .conn-showall {
        align-self: flex-start;
        background: none;
        border: none;
        padding: var(--space-1) 0;
        cursor: pointer;
        color: var(--accent-hover);
        font: inherit;
        font-size: 0.85rem;
        font-weight: 550;
        transition: color var(--transition);
      }
      .conn-showall:hover {
        color: var(--accent);
        text-decoration: underline;
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

  /** Reveal the engines outside the current posture's lane (default: collapsed). */
  readonly showAll = signal(false);

  readonly posture = this.store.posture;
  private readonly inUse = this.store.inUseConnections;

  /** The built-in on-device engine is "in use now" when the posture routes to it. */
  readonly brainInUse = computed(() => this.inUse().has(BRAIN_ENGINE_ID));

  /** The built-in engine belongs to every posture's lane except pure Cloud. */
  readonly brainShown = computed(
    () =>
      this.posture() !== "cloud" ||
      this.brainInUse() ||
      this.showAll(),
  );

  /** Every connection card VM, with its live group + "in use now" flag. */
  private readonly allCards = computed<ConnectionCardVm[]>(() => {
    const statuses = this.store.providers();
    const expanded = this._expanded();
    const ollamaRemote = this.store.ollamaIsRemote();
    const inUse = this.inUse();
    return CONNECTIONS.map((c) => {
      const cloud = c.id === "ollama" ? ollamaRemote : true;
      return {
        ...c,
        status: statuses.find((p) => p.id === c.id) ?? null,
        expanded: expanded.has(c.id),
        cloud,
        inUse: inUse.has(c.id),
      };
    });
  });

  /**
   * Is this connection part of the current posture's default lane? Cloud
   * providers belong to every posture except Fully local; Ollama (a BYO local
   * server) only to Fully local / Custom. An engine actually IN USE is always
   * shown, so the active engine can never be hidden behind "Show all".
   */
  private relevant(id: string): boolean {
    if (this.inUse().has(id)) return true;
    const p = this.posture();
    if (id === "ollama") return p === "fully_local" || p === "custom";
    return p !== "fully_local";
  }

  /** On-this-Mac cards (loopback Ollama) inside the posture's lane, or all when expanded. */
  readonly macCardsShown = computed(() =>
    this.allCards().filter(
      (c) => !c.cloud && (this.relevant(c.id) || this.showAll()),
    ),
  );

  /** Cloud cards inside the posture's lane, or all when expanded. */
  readonly cloudCardsShown = computed(() =>
    this.allCards().filter(
      (c) => c.cloud && (this.relevant(c.id) || this.showAll()),
    ),
  );

  /** How many engines sit OUTSIDE the current posture's lane (0 → no toggle). */
  readonly hiddenCount = computed(() => {
    const hiddenConns = this.allCards().filter(
      (c) => !this.relevant(c.id),
    ).length;
    const brainHidden = this.posture() === "cloud" && !this.brainInUse() ? 1 : 0;
    return hiddenConns + brainHidden;
  });

  toggleShowAll(): void {
    this.showAll.update((v) => !v);
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
