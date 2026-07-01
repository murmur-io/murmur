import {
  ChangeDetectionStrategy,
  Component,
  input,
  output,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type { DocumentInfo } from "../../core/models";

/**
 * One "knowledge source" card on the Brain page (in-flow `.card`, NOT floating).
 *
 * Two shapes, chosen by inputs:
 *  - a READ-ONLY source (Meetings): an emoji + title + count + a link to its
 *    page — no list, no add.
 *  - an EDITABLE source (Documents / Notes): an emoji + title + count, a "+ Add"
 *    button (emitting {@link add}), and an expandable list of items (name +
 *    date + delete) fed by {@link items}. A sealed-selected folder disables the
 *    add + shows a locked note (owned by the parent via {@link blocked}).
 *
 * Pure/presentational: all IPC + folder-state lives in the parent
 * `BrainComponent`; this card only renders + emits `add`/`remove`/`toggle`.
 */
@Component({
  selector: "app-brain-source-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <section class="sc card">
      <header class="sc-head">
        <span class="sc-glyph" aria-hidden="true">{{ glyph() }}</span>
        <div class="sc-head-text">
          <h3 class="sc-title">{{ title() }}</h3>
          <p class="sc-sub">{{ subtitle() }}</p>
        </div>
        <span class="count sc-count" [attr.title]="count() + ' items'">
          {{ count() }}
        </span>
      </header>

      @if (linkTo(); as href) {
        <!-- Read-only source: just a link to its own page. -->
        <a class="btn btn-ghost sc-link" [routerLink]="href">
          {{ linkLabel() }}
          <svg viewBox="0 0 16 16" width="13" height="13" fill="none" aria-hidden="true">
            <path
              d="M5.5 3.5h7v7M12.5 3.5 4 12"
              stroke="currentColor"
              stroke-width="1.6"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </a>
      } @else {
        <!-- Editable source: add affordance + expandable item list. -->
        <div class="sc-actions">
          <button
            type="button"
            class="btn btn-primary sc-add"
            [disabled]="busy() || blocked()"
            (click)="add.emit()"
          >
            @if (busy()) {
              {{ busyLabel() }}
            } @else {
              <svg viewBox="0 0 16 16" width="14" height="14" fill="none" aria-hidden="true">
                <path
                  d="M8 3.5v9M3.5 8h9"
                  stroke="currentColor"
                  stroke-width="1.7"
                  stroke-linecap="round"
                />
              </svg>
              {{ addLabel() }}
            }
          </button>
          @if (items().length > 0) {
            <button
              type="button"
              class="btn btn-ghost sc-toggle"
              [attr.aria-expanded]="expanded()"
              (click)="toggleList.emit()"
            >
              {{ expanded() ? "Hide" : "Show" }} list
            </button>
          }
        </div>

        @if (blocked()) {
          <div class="banner is-accent sc-locked" role="status">
            <span class="sc-locked-glyph" aria-hidden="true"></span>
            <span>
              This folder is locked. Unlock it (in Meetings → folders) to add or
              view its {{ title().toLowerCase() }}.
            </span>
          </div>
        }

        @if (expanded()) {
          @if (loading()) {
            <p class="empty sc-state">Loading…</p>
          } @else if (items().length === 0) {
            <p class="empty sc-state">{{ emptyLabel() }}</p>
          } @else {
            <ul class="sc-list" role="list">
              @for (doc of items(); track doc.id) {
                <li class="sc-item">
                  <span class="sc-item-glyph" aria-hidden="true">{{ glyph() }}</span>
                  <span class="sc-item-text">
                    <span class="sc-item-name">{{ doc.name }}</span>
                    <span class="sc-item-date">{{ formatDate(doc.createdAt) }}</span>
                  </span>
                  <button
                    type="button"
                    class="btn btn-ghost sc-del"
                    [attr.aria-label]="'Delete ' + doc.name"
                    [disabled]="deletingId() === doc.id"
                    (click)="deleteItem.emit(doc)"
                  >
                    <svg viewBox="0 0 16 16" width="14" height="14" fill="none" aria-hidden="true">
                      <path
                        d="M3 4.5h10M6.5 4.5V3.2c0-.4.3-.7.7-.7h1.6c.4 0 .7.3.7.7v1.3M5 4.5l.5 8.3h5L11 4.5"
                        stroke="currentColor"
                        stroke-width="1.3"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                      />
                    </svg>
                  </button>
                </li>
              }
            </ul>
          }
        }
      }
    </section>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .sc {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .sc-head {
        display: flex;
        align-items: flex-start;
        gap: var(--space-3);
      }
      .sc-glyph {
        flex: none;
        font-size: 1.5rem;
        line-height: 1;
      }
      .sc-head-text {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
        flex: 1 1 auto;
      }
      .sc-title {
        margin: 0;
        font-size: 1.0625rem;
      }
      .sc-sub {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.8125rem;
      }
      .sc-count {
        flex: none;
      }

      .sc-link {
        align-self: flex-start;
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
      }

      .sc-actions {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .sc-add {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
      }

      .sc-locked {
        align-items: center;
      }
      .sc-locked-glyph {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: var(--radius-pill);
        background: var(--accent-hover);
        box-shadow: 0 0 0 4px var(--accent-soft);
      }

      .sc-state {
        margin: 0;
        padding: var(--space-2) 0;
      }

      .sc-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .sc-item {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-3);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        transition: border-color var(--transition);
      }
      .sc-item:hover {
        border-color: var(--border-strong);
      }
      .sc-item-glyph {
        flex: none;
        font-size: 1rem;
        line-height: 1;
      }
      .sc-item-text {
        display: flex;
        flex-direction: column;
        gap: 1px;
        min-width: 0;
        flex: 1 1 auto;
      }
      .sc-item-name {
        color: var(--text-primary);
        font-size: 0.9375rem;
        font-weight: 550;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .sc-item-date {
        color: var(--text-muted);
        font-size: 0.75rem;
      }
      .sc-del {
        flex: none;
        width: 34px;
        height: 34px;
        padding: 0;
        color: var(--text-muted);
      }
      .sc-del:hover {
        color: var(--danger);
      }
    `,
  ],
})
export class BrainSourceCardComponent {
  /** The leading emoji glyph (🎙 / 📄 / 📝). */
  readonly glyph = input.required<string>();
  readonly title = input.required<string>();
  readonly subtitle = input.required<string>();
  readonly count = input.required<number>();

  /** When set, the card is a READ-ONLY link source (no list/add). e.g. "/library". */
  readonly linkTo = input<string | null>(null);
  readonly linkLabel = input("Open");

  /** Editable-source inputs (Documents / Notes). */
  readonly items = input<DocumentInfo[]>([]);
  readonly expanded = input(false);
  readonly loading = input(false);
  readonly busy = input(false);
  readonly deletingId = input<string | null>(null);
  /** True when the selected folder is sealed → add disabled + a note. */
  readonly blocked = input(false);
  readonly addLabel = input("Add");
  readonly busyLabel = input("Adding…");
  readonly emptyLabel = input("Nothing here yet.");

  readonly add = output<void>();
  readonly deleteItem = output<DocumentInfo>();
  readonly toggleList = output<void>();

  /** Epoch-millis → a short local date string. */
  protected formatDate(epochMs: number): string {
    return new Date(epochMs).toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }
}
