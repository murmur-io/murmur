import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { IpcService } from "../../core/ipc.service";
import type { UserMemory, UserMemoryFact } from "../../core/models";
import { FoldersService } from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";

/**
 * "What the brain knows about you" — the user-memory audit section on the Brain
 * page. Lists the persisted user-memory FACTS (subject/predicate/object +
 * provenance) that the brain injects into grounding, each with a per-fact
 * "Forget", plus a "Clear all". Data + mutations go through the Phase-3 memory
 * commands ({@link IpcService.getUserMemory} / `forgetUserFact` / `clearUserMemory`).
 *
 * Signals-first + OnPush. GATED server-side: `get_user_memory` only returns facts
 * whose SOURCE meeting is visible under the live unlocked snapshot, so a
 * lock-state change (`folders.tree()`) re-fetches — a sealed-not-unlocked
 * meeting's memory disappears from this list live (mirrors the overview/graph
 * refetch shape in `BrainComponent`). Forget / Clear are bitemporal INVALIDATEs
 * in the backend (history preserved) — the copy says "Forget", not "Delete".
 */
@Component({
  selector: "app-brain-memory",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <section class="mem">
      <button
        type="button"
        class="mem-toggle"
        [attr.aria-expanded]="open()"
        (click)="open.set(!open())"
      >
        <svg
          class="mem-chevron"
          [class.is-open]="open()"
          viewBox="0 0 16 16"
          width="14"
          height="14"
          fill="none"
          aria-hidden="true"
        >
          <path
            d="M5.5 4l5 4-5 4"
            stroke="currentColor"
            stroke-width="1.7"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
        <span class="mem-title">What the brain knows about you</span>
        @if (facts().length > 0) {
          <span class="count mem-count">{{ facts().length }}</span>
        }
        <span class="mem-sub">
          Facts the brain remembers about you and injects into its answers.
        </span>
      </button>

      @if (open()) {
        @if (loading()) {
          <div class="card state-card">
            <p class="empty">Loading what the brain remembers…</p>
          </div>
        } @else if (error()) {
          <div class="card empty-state">
            <span class="empty-mark" aria-hidden="true"></span>
            <p class="empty-title">Couldn’t load your memory</p>
            <p class="empty">{{ error() }}</p>
          </div>
        } @else if (facts().length === 0) {
          <div class="card empty-state">
            <span class="empty-mark" aria-hidden="true"></span>
            <p class="empty-title">The brain hasn’t learned anything yet</p>
            <p class="empty">
              As you record meetings, the brain notes durable facts about you —
              how you like to work, who you work with, what you own — so it can
              answer with context. They’ll appear here for you to review or
              forget. Nothing leaves this Mac unredacted.
            </p>
          </div>
        } @else {
          <div class="card mem-card">
            <ul class="mem-list">
              @for (f of facts(); track f.id) {
                <li class="mem-item">
                  <div class="mem-fact">
                    <span class="mem-fact-text">{{ factLine(f) }}</span>
                    @if (f.sourceMeetingId; as mid) {
                      <a
                        class="mem-source"
                        [routerLink]="['/meeting', mid]"
                        title="Where the brain learned this"
                      >
                        <svg
                          viewBox="0 0 16 16"
                          width="11"
                          height="11"
                          fill="none"
                          aria-hidden="true"
                        >
                          <path
                            d="M4 2.5h5l3 3v8H4z"
                            stroke="currentColor"
                            stroke-width="1.3"
                            stroke-linejoin="round"
                          />
                          <path
                            d="M9 2.5v3h3"
                            stroke="currentColor"
                            stroke-width="1.3"
                            stroke-linejoin="round"
                          />
                        </svg>
                        Source
                      </a>
                    }
                  </div>
                  <button
                    type="button"
                    class="btn btn-ghost mem-forget"
                    (click)="forget(f.id)"
                    [disabled]="forgettingId() === f.id"
                    [attr.aria-label]="'Forget: ' + factLine(f)"
                  >
                    {{ forgettingId() === f.id ? "Forgetting…" : "Forget" }}
                  </button>
                </li>
              }
            </ul>

            <footer class="mem-footer">
              @if (confirmingClear()) {
                <span class="mem-confirm-copy text-secondary">
                  Forget everything the brain has learned about you?
                </span>
                <button
                  type="button"
                  class="btn btn-ghost"
                  (click)="confirmingClear.set(false)"
                  [disabled]="clearing()"
                >
                  Cancel
                </button>
                <button
                  type="button"
                  class="btn btn-danger"
                  (click)="clearAll()"
                  [disabled]="clearing()"
                >
                  {{ clearing() ? "Clearing…" : "Forget all" }}
                </button>
              } @else {
                <button
                  type="button"
                  class="btn btn-ghost mem-clear"
                  (click)="confirmingClear.set(true)"
                >
                  Clear all
                </button>
              }
            </footer>
          </div>
        }
      }
    </section>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .mem {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }

      /* Collapsible header — mirrors the Connections section on the Brain page. */
      .mem-toggle {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        width: 100%;
        padding: var(--space-2) 0;
        border: 0;
        background: transparent;
        color: var(--text-primary);
        font: inherit;
        text-align: left;
        cursor: pointer;
      }
      .mem-toggle:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
        border-radius: var(--radius-sm);
      }
      .mem-chevron {
        flex: none;
        color: var(--text-muted);
        transition: transform var(--transition);
      }
      .mem-chevron.is-open {
        transform: rotate(90deg);
      }
      .mem-title {
        font-size: 1rem;
        font-weight: 600;
        letter-spacing: -0.01em;
      }
      .mem-count {
        flex: none;
      }
      .mem-sub {
        color: var(--text-muted);
        font-size: 0.85rem;
        margin-left: var(--space-2);
      }
      @media (max-width: 560px) {
        .mem-sub {
          display: none;
        }
      }

      .mem-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }

      /* Fact rows: the fact line + a source chip, and a right-aligned Forget. */
      .mem-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .mem-item {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        padding: var(--space-3);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .mem-fact {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .mem-fact-text {
        font-size: 0.925rem;
        line-height: 1.45;
        color: var(--text-primary);
      }
      .mem-source {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        align-self: flex-start;
        color: var(--text-muted);
        font-size: 0.78rem;
        text-decoration: none;
      }
      .mem-source:hover {
        color: var(--accent-hover);
      }
      .mem-forget {
        flex: none;
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.85rem;
      }

      .mem-footer {
        display: flex;
        align-items: center;
        justify-content: flex-end;
        gap: var(--space-3);
        flex-wrap: wrap;
        padding-top: var(--space-2);
        border-top: 1px solid var(--border-subtle);
      }
      .mem-confirm-copy {
        flex: 1 1 auto;
        min-width: 0;
        font-size: 0.875rem;
        line-height: 1.4;
      }
      .mem-clear {
        color: var(--text-muted);
      }
    `,
  ],
})
export class BrainMemoryComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);

  /** Collapsed by default — the Brain page leads with sources, not memory. */
  readonly open = signal(false);

  private readonly memory = signal<UserMemory | null>(null);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);

  readonly facts = computed<UserMemoryFact[]>(
    () => this.memory()?.facts ?? [],
  );

  /** The id of the fact currently being forgotten (disables just that row). */
  readonly forgettingId = signal<string | null>(null);
  /** Clear-all confirm gate + in-flight flag (inline confirm, no floating menu). */
  readonly confirmingClear = signal(false);
  readonly clearing = signal(false);

  constructor() {
    // (Re)load memory whenever the folder lock-state changes — a session
    // unlock/relock shifts which facts are visible (gated by source-meeting
    // visibility server-side). Reading `tree()` registers the dependency; the
    // fetch writes signals synchronously before its first await, so writes must
    // be allowed (NG0600 guard — mirrors BrainComponent's overview effect).
    effect(
      () => {
        this.folders.tree();
        void this.fetch();
      },
      { allowSignalWrites: true },
    );
  }

  /** Render one fact as a plain sentence: "<subject> <predicate> <object>". */
  factLine(f: UserMemoryFact): string {
    return [f.subject, f.predicate, f.object]
      .map((s) => s.trim())
      .filter((s) => s.length > 0)
      .join(" ");
  }

  private async fetch(): Promise<void> {
    this.loading.set(true);
    this.error.set(null);
    try {
      this.memory.set(await this.ipc.getUserMemory());
    } catch (e) {
      this.memory.set(null);
      this.error.set(String(e));
    } finally {
      this.loading.set(false);
    }
  }

  async forget(id: string): Promise<void> {
    if (this.forgettingId()) return;
    this.forgettingId.set(id);
    try {
      await this.ipc.forgetUserFact(id);
      await this.fetch();
      this.toast.info("Forgotten.");
    } catch (e) {
      this.toast.danger(String(e));
    } finally {
      this.forgettingId.set(null);
    }
  }

  async clearAll(): Promise<void> {
    if (this.clearing()) return;
    this.clearing.set(true);
    try {
      await this.ipc.clearUserMemory();
      await this.fetch();
      this.confirmingClear.set(false);
      this.toast.info("Memory cleared.");
    } catch (e) {
      this.toast.danger(String(e));
    } finally {
      this.clearing.set(false);
    }
  }
}
