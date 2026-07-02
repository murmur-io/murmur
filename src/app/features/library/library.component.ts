import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  OnInit,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { IpcService } from "../../core/ipc.service";
import { NavHistoryService } from "../../core/nav-history.service";
import type {
  FolderNode,
  Meeting,
  MeetingStatus,
  SearchHit,
} from "../../core/models";
import {
  FoldersService,
  type FolderExposure,
} from "../../services/folders.service";
import { FolderTreeComponent } from "../folders/folder-tree.component";
import { LockBadgeComponent } from "../folders/lock-badge.component";
import { MoveToMenuComponent } from "../folders/move-to-menu.component";
import { NoteDragService } from "../folders/note-drag.service";
import { ToastService } from "../../services/toast.service";

/** Debounce window for search-as-you-type — quick enough to feel instant. */
const SEARCH_DEBOUNCE_MS = 180;

/** A snippet split into runs around the query match, for safe <mark>-style emphasis. */
interface SnippetPart {
  text: string;
  hit: boolean;
}

@Component({
  selector: "app-library",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  // Esc in Meetings backs out ("← Murmur") — but NOT while you're typing: in the
  // search box Esc clears/blurs it first, and it never hijacks another form field.
  // Declarative host listener — Angular owns its lifecycle (mirrors settings).
  host: { "(document:keydown.escape)": "onEscape()" },
  imports: [
    RouterLink,
    FolderTreeComponent,
    LockBadgeComponent,
    MoveToMenuComponent,
  ],
  template: `
    <section class="library">
      <!-- ============ LEFT PANE — folder tree (lock-aware) ============ -->
      <aside class="folders-pane" aria-label="Folders">
        <!-- Drag strip mirrors the primary rail so the overlay traffic lights
             stay clear of the Back button when the rail is flush to the edge. -->
        <div class="rail-drag" data-tauri-drag-region></div>

        <!-- Drill-down "up": returns to the last non-drill-down route (or /record). -->
        <button
          type="button"
          class="rail-back"
          (click)="nav.back()"
          aria-label="Back to Murmur"
        >
          <svg
            class="rail-back-icon"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M9.5 3.5 5 8l4.5 4.5" />
          </svg>
          <span class="rail-back-label">Murmur</span>
        </button>

        <div class="folders-head">
          <h3 class="folders-title">Folders</h3>
          @if (unlockedCount() > 0) {
            <button
              type="button"
              class="relock-pill"
              [disabled]="relockingAll()"
              [attr.aria-label]="
                'Re-seal all ' + unlockedCount() + ' unlocked folders now'
              "
              title="Re-seal every unlocked folder now"
              (click)="relockAll()"
            >
              <svg
                viewBox="0 0 16 16"
                width="12"
                height="12"
                fill="none"
                aria-hidden="true"
              >
                <rect
                  x="3.5"
                  y="7"
                  width="9"
                  height="6"
                  rx="1.4"
                  stroke="currentColor"
                  stroke-width="1.4"
                />
                <path
                  d="M5.5 7V5.4a2.5 2.5 0 0 1 5 0V7"
                  stroke="currentColor"
                  stroke-width="1.4"
                  stroke-linecap="round"
                />
              </svg>
              Lock all
            </button>
          }
        </div>
        <div class="folders-body">
          @if (foldersLoading()) {
            <p class="folders-state empty">Loading folders…</p>
          } @else {
            <app-folder-tree
              [nodes]="folderTree()"
              [selectedId]="activeFolderId()"
              (select)="selectFolder($event)"
              (dropNote)="onDropNote($event)"
            />
          }
        </div>
      </aside>

      <!-- ============ RIGHT PANE — search, filters, meeting list ============ -->
      <div class="meetings-pane">
        <!-- Frosted search field, pinned to the top of the screen -->
        <div class="search" [class.is-active]="searching()">
          <span class="search-icon" aria-hidden="true">
            <svg viewBox="0 0 20 20" width="18" height="18" fill="none">
              <circle
                cx="8.5"
                cy="8.5"
                r="5.5"
                stroke="currentColor"
                stroke-width="1.7"
              />
              <path
                d="m13 13 4 4"
                stroke="currentColor"
                stroke-width="1.7"
                stroke-linecap="round"
              />
            </svg>
          </span>
          <input
            #searchInput
            type="search"
            class="search-input"
            placeholder="Search meetings, transcripts & notes…"
            autocapitalize="off"
            autocomplete="off"
            spellcheck="false"
            aria-label="Search meetings"
            [value]="query()"
            (input)="onQueryInput($event)"
          />
          @if (query()) {
            <button
              type="button"
              class="search-clear"
              aria-label="Clear search"
              (click)="clear()"
            >
              <svg
                viewBox="0 0 16 16"
                width="14"
                height="14"
                aria-hidden="true"
              >
                <path
                  d="M4 4l8 8M12 4l-8 8"
                  stroke="currentColor"
                  stroke-width="1.7"
                  stroke-linecap="round"
                />
              </svg>
            </button>
          }
        </div>

        <!-- Tag filter chips (hidden during an active search; absent if no tags) -->
        @if (!hasQuery() && tags().length > 0) {
          <div class="tagbar" role="group" aria-label="Filter meetings by tag">
            <button
              type="button"
              class="chip"
              [class.is-active]="activeTag() === null"
              [attr.aria-pressed]="activeTag() === null"
              (click)="selectTag(null)"
            >
              All
            </button>
            @for (tag of tags(); track tag) {
              <button
                type="button"
                class="chip"
                [class.is-active]="activeTag() === tag"
                [attr.aria-pressed]="activeTag() === tag"
                (click)="selectTag(tag)"
              >
                {{ tag }}
              </button>
            }
          </div>
        }

        @if (hasQuery()) {
          <!-- ===================== SEARCH RESULTS ===================== -->
          <header class="library-head">
            <h2>Search</h2>
            @if (!searching() && results().length > 0) {
              <span class="count">{{ results().length }}</span>
            }
          </header>

          @if (searching()) {
            <div class="card state-card">
              <p class="empty searching">
                <span class="spinner" aria-hidden="true"></span>
                Searching…
              </p>
            </div>
          } @else if (results().length === 0) {
            <div class="card empty-state">
              <span class="empty-mark" aria-hidden="true"></span>
              <p class="empty-title">No matches for “{{ query().trim() }}”</p>
              <p class="empty">Try a different word or a shorter phrase.</p>
            </div>
          } @else {
            <ul class="list card">
              @for (hit of results(); track hit.meeting.id; let i = $index) {
                <li>
                  <a
                    class="row"
                    [routerLink]="['/meeting', hit.meeting.id]"
                    [style.animation-delay.ms]="i * 35"
                  >
                    <span class="row-main">
                      <span class="title">{{
                        hit.meeting.title || "(untitled)"
                      }}</span>
                      <span class="meta">
                        <span
                          class="badge"
                          [class]="matchBadgeClass(hit.matchedIn)"
                          >{{ matchLabel(hit.matchedIn) }}</span
                        >
                        <span class="date">{{
                          formatDate(hit.meeting.startedAt)
                        }}</span>
                      </span>
                      @if (hit.snippet) {
                        <span class="snippet">
                          @for (
                            part of snippetParts(hit.snippet);
                            track $index
                          ) {
                            @if (part.hit) {
                              <mark class="snippet-hit">{{ part.text }}</mark>
                            } @else {
                              {{ part.text }}
                            }
                          }
                        </span>
                      }
                    </span>
                    <span class="chevron" aria-hidden="true">›</span>
                  </a>
                </li>
              }
            </ul>
          }
        } @else {
          <!-- ===================== MEETINGS LIST (no query) ===================== -->
          <header class="library-head">
            <h2>{{ listHeading() }}</h2>
            @if (activeFolderExposure(); as exp) {
              <app-lock-badge [exposure]="exp" />
            }
            @if (!listLoading() && displayedMeetings().length > 0) {
              <span class="count">{{ displayedMeetings().length }}</span>
            }
          </header>

          @if (listLoading()) {
            <div class="card state-card">
              <p class="empty">Loading…</p>
            </div>
          } @else if (displayedMeetings().length === 0) {
            <div class="card empty-state">
              @if (activeFolderId() !== null) {
                <!-- ACTIONABLE empty folder: on-brand "drop here" illustration
                     plus the two concrete ways to file a note. -->
                <span class="empty-illo" aria-hidden="true">
                  <svg viewBox="0 0 64 64" width="64" height="64" fill="none">
                    <defs>
                      <linearGradient
                        id="emptyFolderGrad"
                        x1="8"
                        y1="14"
                        x2="56"
                        y2="52"
                        gradientUnits="userSpaceOnUse"
                      >
                        <stop stop-color="#6e76ff" />
                        <stop offset="1" stop-color="#9d7bff" />
                      </linearGradient>
                    </defs>
                    <!-- open folder, dashed mouth inviting a drop -->
                    <path
                      d="M9 22c0-2.2 1.8-4 4-4h9.5c1.3 0 2.5.6 3.3 1.7l1.6 2.3H51c2.2 0 4 1.8 4 4v20c0 2.2-1.8 4-4 4H13c-2.2 0-4-1.8-4-4z"
                      stroke="url(#emptyFolderGrad)"
                      stroke-width="2.2"
                      stroke-linejoin="round"
                    />
                    <path
                      d="M14 31h36"
                      stroke="url(#emptyFolderGrad)"
                      stroke-width="2"
                      stroke-linecap="round"
                      stroke-dasharray="3 4"
                      opacity="0.7"
                    />
                    <!-- a note card dropping in, with the soundwave mark -->
                    <g opacity="0.95">
                      <rect
                        x="24"
                        y="9"
                        width="16"
                        height="20"
                        rx="3"
                        fill="var(--surface-overlay)"
                        stroke="url(#emptyFolderGrad)"
                        stroke-width="2"
                      />
                      <path
                        d="M28 19v0M31 16.5v5M34 14v10M37 17.5v3"
                        stroke="url(#emptyFolderGrad)"
                        stroke-width="2"
                        stroke-linecap="round"
                      />
                    </g>
                  </svg>
                </span>
                <p class="empty-title">Nothing filed here yet</p>
                <p class="empty">
                  Drag a meeting onto this folder, or use the
                  <span class="empty-chip-hint">
                    <svg
                      viewBox="0 0 16 16"
                      width="11"
                      height="11"
                      fill="none"
                      aria-hidden="true"
                    >
                      <path
                        d="M1.75 4.25c0-.7.55-1.25 1.25-1.25h2.8c.4 0 .77.18 1 .5l.6.75h4.6c.7 0 1.25.55 1.25 1.25v5.5c0 .7-.55 1.25-1.25 1.25H3c-.7 0-1.25-.55-1.25-1.25z"
                        stroke="currentColor"
                        stroke-width="1.3"
                        stroke-linejoin="round"
                      />
                    </svg>
                    folder button</span
                  >
                  on any meeting to move it in.
                </p>
              } @else if (activeTag() === null) {
                <span class="empty-mark" aria-hidden="true"></span>
                <p class="empty-title">No meetings yet</p>
                <p class="empty">
                  Record one from the Record tab to see it here.
                </p>
              } @else {
                <span class="empty-mark" aria-hidden="true"></span>
                <p class="empty-title">
                  No meetings tagged “{{ activeTag() }}”
                </p>
                <p class="empty">Pick another tag, or choose All.</p>
              }
            </div>
          } @else {
            <ul class="list card">
              @for (m of displayedMeetings(); track m.id; let i = $index) {
                <li
                  class="row-item"
                  [class.is-confirming]="pendingDeleteId() === m.id"
                  [class.is-dragging]="draggingId() === m.id"
                  [class.is-menu-open]="movePopoverId() === m.id"
                >
                  <a
                    class="row"
                    [routerLink]="['/meeting', m.id]"
                    [style.animation-delay.ms]="i * 45"
                    draggable="true"
                    (dragstart)="onRowDragStart($event, m)"
                    (dragend)="onRowDragEnd()"
                  >
                    <!-- Drag grip: signals the row is draggable; not a button so
                         it never steals the row's navigation click. -->
                    <span
                      class="grip"
                      aria-hidden="true"
                      title="Drag to a folder"
                    >
                      <svg viewBox="0 0 16 16" width="14" height="14">
                        <circle cx="6" cy="4" r="1.05" fill="currentColor" />
                        <circle cx="10" cy="4" r="1.05" fill="currentColor" />
                        <circle cx="6" cy="8" r="1.05" fill="currentColor" />
                        <circle cx="10" cy="8" r="1.05" fill="currentColor" />
                        <circle cx="6" cy="12" r="1.05" fill="currentColor" />
                        <circle cx="10" cy="12" r="1.05" fill="currentColor" />
                      </svg>
                    </span>

                    <span class="row-main">
                      <span class="title-row">
                        @if (isMasked(m)) {
                          <span class="title title--masked" aria-hidden="true"
                            >•••••••••••••</span
                          >
                          <span class="sr-only"
                            >Locked note — title hidden</span
                          >
                        } @else {
                          <span class="title">{{
                            m.title || "(untitled)"
                          }}</span>
                        }
                      </span>
                      <span class="meta">
                        <span class="date">{{ formatDate(m.startedAt) }}</span>
                        @if (m.durationS > 0) {
                          <span class="dot" aria-hidden="true">·</span>
                          <span class="duration">{{
                            formatDuration(m.durationS)
                          }}</span>
                        }
                      </span>
                    </span>
                    <span class="row-aside">
                      <!-- FOLDER CHIP — the primary "choose which goes where"
                           affordance. Shows the current folder (or "+ Add to
                           folder" at root). A button (stop/prevent) so the row's
                           link never fires; opens the Move-to popover below. -->
                      <button
                        type="button"
                        class="folder-chip"
                        [class.is-filed]="folderNameOf(m) !== null"
                        [attr.aria-expanded]="movePopoverId() === m.id"
                        aria-haspopup="menu"
                        [attr.aria-label]="
                          folderNameOf(m)
                            ? 'In folder ' +
                              folderNameOf(m) +
                              ' — move to another folder'
                            : 'Add this meeting to a folder'
                        "
                        (click)="
                          $event.preventDefault();
                          $event.stopPropagation();
                          toggleMovePopover(m.id)
                        "
                      >
                        @if (folderExposureOf(m); as exp) {
                          <app-lock-badge [exposure]="exp" />
                        } @else {
                          <svg
                            class="folder-chip-icon"
                            viewBox="0 0 16 16"
                            width="13"
                            height="13"
                            fill="none"
                            aria-hidden="true"
                          >
                            <path
                              d="M1.75 4.25c0-.7.55-1.25 1.25-1.25h2.8c.4 0 .77.18 1 .5l.6.75h4.6c.7 0 1.25.55 1.25 1.25v5.5c0 .7-.55 1.25-1.25 1.25H3c-.7 0-1.25-.55-1.25-1.25z"
                              stroke="currentColor"
                              stroke-width="1.3"
                              stroke-linejoin="round"
                            />
                          </svg>
                        }
                        <span class="folder-chip-label">{{
                          folderNameOf(m) ?? "Add to folder"
                        }}</span>
                        @if (folderNameOf(m) === null) {
                          <span class="folder-chip-plus" aria-hidden="true"
                            >+</span
                          >
                        }
                      </button>

                      <span class="pill" [class]="statusPillClass(m.status)">
                        <span class="pill-dot"></span>
                        {{ statusLabel(m.status) }}
                      </span>
                      <span class="chevron" aria-hidden="true">›</span>
                    </span>
                  </a>

                  <!-- "Move to…" popover, anchored to the row's right edge. The
                       picker owns the cross-encryption-boundary confirm + IPC; we
                       just reconcile the local list on its moved event. -->
                  @if (movePopoverId() === m.id) {
                    <div class="move-anchor">
                      <app-move-to-menu
                        [meetingId]="m.id"
                        [currentFolderId]="m.folderId ?? null"
                        (moved)="onMoved(m.id, $event)"
                        (close)="closeMovePopover()"
                      />
                    </div>
                  }

                  <!-- Subtle delete affordance — a separate button (never the
                     row's link). stop/prevent so a click can't navigate. -->
                  <button
                    type="button"
                    class="row-delete"
                    [attr.aria-label]="
                      'Delete meeting: ' + (m.title || '(untitled)')
                    "
                    (click)="
                      $event.preventDefault();
                      $event.stopPropagation();
                      askDelete(m.id)
                    "
                  >
                    <svg
                      viewBox="0 0 16 16"
                      width="15"
                      height="15"
                      aria-hidden="true"
                    >
                      <path
                        d="M3 4.5h10M6.5 4.5V3.5a1 1 0 0 1 1-1h1a1 1 0 0 1 1 1v1M5.5 4.5l.4 8a1 1 0 0 0 1 .95h2.2a1 1 0 0 0 1-.95l.4-8"
                        stroke="currentColor"
                        stroke-width="1.3"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                        fill="none"
                      />
                    </svg>
                  </button>

                  @if (pendingDeleteId() === m.id) {
                    <!-- In-app confirm (signal-driven; NOT window.confirm) -->
                    <div
                      class="confirm"
                      role="alertdialog"
                      aria-modal="true"
                      [attr.aria-label]="
                        'Delete meeting: ' + (m.title || '(untitled)')
                      "
                    >
                      <p class="confirm-title">Delete this meeting?</p>
                      <p class="confirm-body">
                        This permanently removes the recording, transcript,
                        summary and vault note. It can’t be undone.
                      </p>
                      @if (deleteError()) {
                        <p class="confirm-error" role="alert">
                          {{ deleteError() }}
                        </p>
                      }
                      <div class="confirm-actions">
                        <button
                          type="button"
                          class="btn btn-ghost"
                          [disabled]="deleting()"
                          (click)="cancelDelete()"
                        >
                          Cancel
                        </button>
                        <button
                          type="button"
                          class="btn btn-danger"
                          [disabled]="deleting()"
                          (click)="confirmDelete(m.id)"
                        >
                          @if (deleting()) {
                            <span class="spinner" aria-hidden="true"></span>
                            Deleting…
                          } @else {
                            Delete
                          }
                        </button>
                      </div>
                    </div>
                  }
                </li>
              }
            </ul>
          }
        }
      </div>
      <!-- /.meetings-pane -->
    </section>
  `,
  styles: [
    `
      /* Meetings is a full drill-down (L2): app-shell hides the primary rail and
         this fixed host fills the window as a flush-left [folders rail | content]
         layout, below the toast viewport (z 60). Mirrors settings.component. */
      :host {
        position: fixed;
        inset: 0;
        z-index: 5;
        display: block;
        background: var(--surface-base);
      }

      /* Two-pane shell: full-height folders rail + scrolling meetings content.
         Collapses to stacked rows on narrow widths so the list never squeezes. */
      .library {
        display: grid;
        grid-template-columns: 248px minmax(0, 1fr);
        height: 100vh;
        height: 100dvh;
      }

      /* --- Left pane: folder tree (lock-aware) ---
         A first-class full-height column flush to the window edge, same visual
         weight as the primary rail it replaces (frosted in-flow chrome, right
         border, NOT a floating card). Mirrors .settings-sidebar. */
      .folders-pane {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        height: 100%;
        padding: 0 var(--space-3) var(--space-4);
        background: var(--surface-raised);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        border-right: 1px solid var(--border-subtle);
        box-shadow: var(--glass-highlight);
        overflow: hidden;
        animation: library-enter 300ms cubic-bezier(0.22, 1, 0.36, 1) both;
      }

      /* Enter transition — rail + meetings pane share ONE eased glide (content
         lags 40ms). TRANSFORM ONLY, never opacity: the fixed :host is opaque
         near-black (--surface-base), so an opacity fade would flash black then
         jump the UI in (the bug fixed for settings). Painted opaque from frame 1,
         it just settles into place. Disabled under reduced-motion below. */
      @keyframes library-enter {
        from {
          transform: translateY(8px);
        }
        to {
          transform: none;
        }
      }

      /* Top drag strip — mirrors the primary rail so the overlay traffic lights
         have somewhere to float and the window stays draggable up here. */
      .rail-drag {
        flex: none;
        height: 30px;
      }

      /* "← Murmur" drill-down up-affordance. */
      .rail-back {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        width: 100%;
        height: 34px;
        padding: 0 var(--space-2);
        border: 0;
        border-radius: var(--radius-md);
        background: transparent;
        color: var(--text-secondary);
        font: inherit;
        font-size: 0.9rem;
        font-weight: 600;
        letter-spacing: -0.01em;
        text-align: left;
        cursor: pointer;
        transition:
          background var(--transition-fast),
          color var(--transition-fast);
      }
      .rail-back:hover {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      .rail-back:focus-visible {
        outline: none;
        color: var(--text-primary);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .rail-back-icon {
        flex: none;
        width: 16px;
        height: 16px;
      }

      /* Folder tree scrolls independently within the fixed full-height rail. */
      .folders-body {
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
      }

      .folders-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-2);
        margin-bottom: var(--space-2);
        min-height: 24px;
      }
      .folders-title {
        margin: 0;
        padding: 0 var(--space-2);
        color: var(--text-muted);
        font-size: 0.75rem;
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.04em;
      }
      /* "Lock all" — quiet accent pill, only present when a folder is exposed. */
      .relock-pill {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        height: 24px;
        padding: 0 var(--space-2);
        border: 1px solid transparent;
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        color: var(--accent-hover);
        font-family: inherit;
        font-size: 0.7rem;
        font-weight: 600;
        letter-spacing: 0.01em;
        cursor: pointer;
        transition:
          filter var(--transition),
          transform var(--transition-fast);
      }
      .relock-pill:hover:not(:disabled) {
        filter: brightness(1.12);
      }
      .relock-pill:active:not(:disabled) {
        transform: scale(0.95);
      }
      .relock-pill:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .relock-pill:disabled {
        opacity: 0.5;
        cursor: not-allowed;
      }
      .folders-state {
        margin: var(--space-2);
        font-size: 0.8125rem;
      }

      /* --- Right pane: stacks search, filters and the list ---
         Scrolls independently, with its own padding (no longer inside .app-main's
         padded column). Shares the rail's transform-only glide (40ms lag). */
      .meetings-pane {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        min-width: 0;
        height: 100%;
        overflow-y: auto;
        padding: var(--space-6) var(--space-6) var(--space-8);
        animation: library-enter 300ms cubic-bezier(0.22, 1, 0.36, 1) 40ms both;
      }

      /* Masked title for a locked-folder note (hidden until session-unlock). */
      .title-row {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        min-width: 0;
      }

      /* --- Drag grip (signals a row is draggable; reveals on row hover) ----- */
      .grip {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 18px;
        margin-left: calc(var(--space-2) * -1);
        color: var(--text-muted);
        cursor: grab;
        opacity: 0;
        transition:
          opacity var(--transition),
          color var(--transition);
      }
      .row:hover .grip,
      .row:focus-visible .grip {
        opacity: 0.65;
      }
      .row:hover .grip:hover {
        opacity: 1;
        color: var(--text-secondary);
      }
      .row[draggable="true"]:active .grip {
        cursor: grabbing;
      }

      /* --- Folder chip — the primary filing affordance on every row -------- */
      .folder-chip {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        max-width: 168px;
        height: 26px;
        padding: 0 var(--space-2);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        color: var(--text-muted);
        font-family: inherit;
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: -0.005em;
        line-height: 1;
        white-space: nowrap;
        cursor: pointer;
        transition:
          color var(--transition),
          background var(--transition),
          border-color var(--transition),
          transform var(--transition-fast);
      }
      .folder-chip:hover {
        color: var(--text-primary);
        background: var(--surface-hover);
        border-color: var(--border-strong);
      }
      .folder-chip:active {
        transform: scale(0.96);
      }
      .folder-chip:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      /* A note that IS filed reads in the accent voice — its folder is "real". */
      .folder-chip.is-filed {
        background: var(--accent-soft);
        border-color: transparent;
        color: var(--accent-hover);
      }
      .folder-chip.is-filed:hover {
        background: var(--accent-soft);
        filter: brightness(1.12);
      }
      .row-item.is-menu-open .folder-chip {
        background: var(--accent-soft);
        border-color: var(--accent);
        color: var(--accent-hover);
      }
      .folder-chip-icon {
        flex: none;
      }
      .folder-chip-label {
        overflow: hidden;
        text-overflow: ellipsis;
      }
      .folder-chip-plus {
        font-size: 0.95rem;
        line-height: 0;
        margin-left: 1px;
        opacity: 0.8;
      }

      /* --- "Move to…" popover anchor (right edge of the row) --------------- */
      .move-anchor {
        position: absolute;
        top: calc(100% - var(--space-2));
        right: var(--space-3);
        z-index: 40;
        animation: rise 160ms var(--transition) both;
      }

      /* --- Drag source: dim the row being dragged for a tidy affordance ---- */
      .row-item.is-dragging {
        opacity: 0.45;
      }
      .row-item.is-dragging .row {
        background: var(--surface-hover);
      }

      /* --- Actionable empty-folder illustration --------------------------- */
      .empty-illo {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        margin-bottom: var(--space-3);
        filter: drop-shadow(0 8px 24px rgba(110, 118, 255, 0.25));
        animation: rise 420ms var(--transition) both;
      }
      .empty-chip-hint {
        display: inline-flex;
        align-items: center;
        gap: 3px;
        padding: 1px var(--space-2);
        margin: 0 2px;
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        color: var(--accent-hover);
        font-weight: 600;
        white-space: nowrap;
        vertical-align: baseline;
      }
      .title--masked {
        color: var(--text-muted);
        letter-spacing: 0.12em;
        user-select: none;
      }
      .sr-only {
        position: absolute;
        width: 1px;
        height: 1px;
        margin: -1px;
        padding: 0;
        border: 0;
        overflow: hidden;
        clip: rect(0 0 0 0);
        clip-path: inset(50%);
        white-space: nowrap;
      }

      /* --- Frosted search field (pinned, sticky to the scroll top) ---
         Sticks flush to the top of the meetings pane's scroll area; the negative
         top pulls it over the pane's own top padding so it hugs the edge as the
         list scrolls beneath it. */
      .search {
        position: sticky;
        top: calc(var(--space-6) * -1);
        z-index: 5;
        display: flex;
        align-items: center;
        gap: var(--space-2);
        padding: 0 var(--space-3);
        height: 48px;
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        box-shadow: var(--shadow-sm), var(--glass-highlight);
        transition:
          border-color var(--transition),
          box-shadow var(--transition);
      }
      .search:focus-within {
        border-color: var(--accent);
        box-shadow:
          0 0 0 3px var(--accent-ring),
          var(--glass-highlight);
      }
      .search-icon {
        display: inline-flex;
        flex: none;
        color: var(--text-muted);
        transition: color var(--transition);
      }
      .search:focus-within .search-icon,
      .search.is-active .search-icon {
        color: var(--accent-hover);
      }
      /* Reset the global input chrome — the wrapper carries the frosting. */
      .search-input {
        flex: 1 1 auto;
        min-width: 0;
        height: 100%;
        padding: 0;
        border: none;
        border-radius: 0;
        background: transparent;
        color: var(--text-primary);
        font-size: 0.9375rem;
        letter-spacing: -0.01em;
      }
      .search-input:hover,
      .search-input:focus {
        border: none;
        background: transparent;
        box-shadow: none;
      }
      .search-input::placeholder {
        color: var(--text-muted);
      }
      /* Hide the native WebKit search affordances — we draw our own ✕. */
      .search-input::-webkit-search-decoration,
      .search-input::-webkit-search-cancel-button {
        -webkit-appearance: none;
        appearance: none;
      }
      .search-clear {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 28px;
        height: 28px;
        padding: 0;
        border: none;
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        color: var(--text-muted);
        cursor: pointer;
        transition:
          background var(--transition),
          color var(--transition),
          transform var(--transition-fast);
      }
      .search-clear:hover {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      .search-clear:active {
        transform: scale(0.92);
      }
      .search-clear:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }

      /* --- Tag filter chips (pill language; accent for the active tag) ----- */
      .tagbar {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
      }
      .chip {
        display: inline-flex;
        align-items: center;
        height: 30px;
        padding: 0 var(--space-3);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: -0.01em;
        line-height: 1;
        white-space: nowrap;
        cursor: pointer;
        transition:
          background var(--transition),
          border-color var(--transition),
          color var(--transition),
          transform var(--transition-fast);
      }
      .chip:hover {
        background: var(--surface-hover);
        border-color: var(--border-strong);
        color: var(--text-primary);
      }
      .chip:active {
        transform: scale(0.96);
      }
      .chip:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .chip.is-active {
        background: var(--accent-soft);
        border-color: transparent;
        color: var(--accent-hover);
        font-weight: 600;
      }

      .library-head {
        display: flex;
        align-items: center;
        gap: var(--space-3);
      }
      .library-head h2 {
        margin: 0;
      }

      /* --- Meeting / results list --- */
      .list {
        list-style: none;
        padding: var(--space-2);
        margin: 0;
      }
      .row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        padding: var(--space-3) var(--space-4);
        border-radius: var(--radius-md);
        text-decoration: none;
        color: inherit;
        animation: rise 360ms var(--transition) both;
        transition:
          background var(--transition),
          transform var(--transition-fast);
      }
      .list li + li {
        border-top: 1px solid var(--border-subtle);
      }
      .row:hover {
        background: var(--surface-hover);
      }
      .row:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .row:active {
        transform: translateY(1px);
      }

      /* --- Per-row delete affordance (hidden until hover / focus) --------- */
      .row-item {
        position: relative;
      }
      /* Keep the link clear of the delete button's hover footprint. */
      .row-item .row {
        padding-right: var(--space-7);
      }
      .row-delete {
        position: absolute;
        top: 50%;
        right: var(--space-3);
        transform: translateY(-50%) scale(0.9);
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 30px;
        height: 30px;
        padding: 0;
        border: 1px solid transparent;
        border-radius: var(--radius-sm);
        background: var(--surface-input);
        color: var(--text-muted);
        cursor: pointer;
        opacity: 0;
        pointer-events: none;
        transition:
          opacity var(--transition),
          transform var(--transition-fast),
          background var(--transition),
          border-color var(--transition),
          color var(--transition);
      }
      .row-item:hover .row-delete,
      .row-item:focus-within .row-delete,
      .row-item.is-confirming .row-delete {
        opacity: 1;
        pointer-events: auto;
        transform: translateY(-50%) scale(1);
      }
      .row-delete:hover {
        background: var(--danger-soft);
        border-color: var(--danger);
        color: var(--danger);
      }
      .row-delete:active {
        transform: translateY(-50%) scale(0.92);
      }
      .row-delete:focus-visible {
        outline: none;
        opacity: 1;
        pointer-events: auto;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      /* Fade the decorative chevron out so the ✕ owns the right edge. */
      .row-item:hover .chevron,
      .row-item:focus-within .chevron,
      .row-item.is-confirming .chevron {
        opacity: 0;
      }

      /* --- In-app delete confirm (signal-driven; not window.confirm) ------ */
      .confirm {
        margin: 0 var(--space-2) var(--space-2);
        padding: var(--space-4);
        border: 1px solid var(--danger);
        border-radius: var(--radius-md);
        background: var(--danger-soft);
        animation: rise 200ms var(--transition) both;
      }
      .confirm-title {
        margin: 0 0 var(--space-1);
        color: var(--text-primary);
        font-weight: 600;
      }
      .confirm-body {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.875rem;
        line-height: 1.5;
      }
      .confirm-error {
        margin: var(--space-3) 0 0;
        color: var(--danger);
        font-size: 0.8125rem;
      }
      .confirm-actions {
        display: flex;
        justify-content: flex-end;
        gap: var(--space-2);
        margin-top: var(--space-4);
      }

      .row-main {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .title {
        color: var(--text-primary);
        font-weight: 600;
        letter-spacing: -0.01em;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .meta {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-muted);
        font-size: 0.8125rem;
      }
      .meta .dot {
        color: var(--text-muted);
      }
      .date {
        font-family: var(--font-mono);
        font-variant-numeric: tabular-nums;
        letter-spacing: -0.01em;
      }
      .duration {
        font-family: var(--font-mono);
        font-variant-numeric: tabular-nums;
      }

      .row-aside {
        display: inline-flex;
        align-items: center;
        gap: var(--space-3);
        flex: none;
      }
      .chevron {
        color: var(--text-muted);
        font-size: 1.25rem;
        line-height: 1;
        transition:
          color var(--transition),
          opacity var(--transition),
          transform var(--transition);
      }
      .row:hover .chevron {
        color: var(--text-secondary);
        transform: translateX(2px);
      }

      /* --- Search result extras: matched-in badge + snippet --- */
      .badge {
        display: inline-flex;
        align-items: center;
        height: 18px;
        padding: 0 var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-size: 0.6875rem;
        font-weight: 600;
        letter-spacing: 0.02em;
        text-transform: uppercase;
        line-height: 1;
        flex: none;
      }
      .badge.is-accent {
        background: var(--accent-soft);
        border-color: transparent;
        color: var(--accent-hover);
      }
      .badge.is-success {
        background: var(--success-soft);
        border-color: transparent;
        color: var(--success);
      }
      .snippet {
        display: -webkit-box;
        -webkit-line-clamp: 2;
        -webkit-box-orient: vertical;
        overflow: hidden;
        color: var(--text-muted);
        font-size: 0.8125rem;
        line-height: 1.5;
      }
      .snippet-hit {
        background: var(--accent-soft);
        color: var(--text-primary);
        border-radius: 3px;
        padding: 0 2px;
      }

      /* --- Loading states (.count/.state-card/.empty* are global) --- */
      .searching {
        display: inline-flex;
        align-items: center;
        gap: var(--space-3);
      }
      .spinner {
        width: 16px;
        height: 16px;
        flex: none;
        border-radius: 50%;
        border: 2px solid var(--border-strong);
        border-top-color: var(--accent-hover);
        animation: spin 700ms linear infinite;
      }
      @keyframes spin {
        to {
          transform: rotate(360deg);
        }
      }

      /* Narrow widths: stack the folders rail on top of the meetings content
         (rows), each scrolling within the fixed full-height shell. Mirrors the
         settings drill-down's narrow layout. */
      @media (max-width: 720px) {
        .library {
          grid-template-columns: 1fr;
          grid-template-rows: auto minmax(0, 1fr);
        }
        .folders-pane {
          border-right: 0;
          border-bottom: 1px solid var(--border-subtle);
        }
        .meetings-pane {
          padding: var(--space-5) var(--space-4) var(--space-6);
        }
        .search {
          top: calc(var(--space-5) * -1);
        }
      }

      /* Honor reduced-motion everywhere: no rail slide / content stagger (the
         surface is opaque from frame 1, so entry is instant with no flash), and
         no in-flow rise on the popover / empty illustration. */
      @media (prefers-reduced-motion: reduce) {
        .folders-pane,
        .meetings-pane,
        .move-anchor,
        .empty-illo {
          animation: none;
        }
      }
    `,
  ],
})
export class LibraryComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly folders = inject(FoldersService);
  private readonly drag = inject(NoteDragService);
  private readonly toast = inject(ToastService);

  /** Drill-down back navigation ("← Murmur" + Esc) — no library state coupling. */
  readonly nav = inject(NavHistoryService);

  /**
   * Esc while in Meetings. Backs out to where you came from — EXCEPT while you're
   * typing: in the search box the first Esc clears it (or blurs when empty), and
   * Esc is ignored inside any other form field, so it never ejects you mid-edit.
   * Mirrors settings.component's onEscape.
   */
  onEscape(): void {
    const el = document.activeElement as HTMLElement | null;
    if (el?.classList.contains("search-input")) {
      if (this.query().trim()) {
        this.clear();
      } else {
        el.blur();
      }
      return;
    }
    const tag = el?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") {
      return;
    }
    this.nav.back();
  }

  /** The meeting id whose "Move to…" folder-chip popover is open (null = none). */
  readonly movePopoverId = signal<string | null>(null);

  /** The meeting id currently being dragged (mirrors the shared drag signal). */
  readonly draggingId = this.drag.draggingId;

  /** The search box element — focused after a clear. */
  private readonly searchInput =
    viewChild<ElementRef<HTMLInputElement>>("searchInput");

  // --- No-query meetings list (unchanged behaviour) -----------------------
  readonly meetings = signal<Meeting[]>([]);
  readonly loading = signal(true);

  // --- Folder filter (left pane) ------------------------------------------
  /** The lock-aware folder forest from the signal store. */
  readonly folderTree = this.folders.tree;
  /** True while the folder tree is loading (drives the left-pane state). */
  readonly foldersLoading = this.folders.loading;
  /** How many sealed folders are session-unlocked right now (drives "Lock all"). */
  readonly unlockedCount = this.folders.unlockedCount;
  /** True while a "Lock all" op is in flight. */
  readonly relockingAll = signal(false);
  /**
   * Selected folder id (null = no folder filter — show the tag/all list).
   * Mutually exclusive with the tag filter: selecting one clears the other.
   */
  readonly activeFolderId = signal<string | null>(null);

  /**
   * Every folder node keyed by id (flattened) — for O(1) exposure/mask lookups
   * keyed off a meeting's `folderId`. Recomputes whenever the tree reloads.
   */
  private readonly folderById = computed(() => {
    const map = new Map<string, FolderNode>();
    const walk = (nodes: FolderNode[]): void => {
      for (const n of nodes) {
        map.set(n.id, n);
        // Defensive: a node from an older/odd backend may omit `children`.
        // Never let a missing array throw here — that would take the whole
        // Library view (both panes) down, not just the folder tree.
        if (n.children?.length) {
          walk(n.children);
        }
      }
    };
    walk(this.folderTree());
    return map;
  });

  /**
   * Meetings in the active folder. Derived from the already-loaded `meetings`
   * list via the committed `Meeting.folderId` contract field (no extra IPC —
   * the backend exposes no folder-scoped list command). When the field is
   * absent (older backend) this is simply empty until notes carry a folderId.
   */
  readonly folderMeetings = computed(() => {
    const fid = this.activeFolderId();
    if (fid === null) {
      return [];
    }
    return this.meetings().filter((m) => m.folderId === fid);
  });

  // --- Tag filter ----------------------------------------------------------
  /** All distinct tags across meetings; empty → no filter bar is rendered. */
  readonly tags = signal<string[]>([]);
  /** Selected tag (null = "All", i.e. the full meetings list). */
  readonly activeTag = signal<string | null>(null);
  /** Meetings carrying the active tag (only used when a tag is selected). */
  readonly tagMeetings = signal<Meeting[]>([]);
  /** True while a tag's meetings are being fetched. */
  readonly tagLoading = signal(false);

  /**
   * The list to render when not searching, in strict precedence (search is
   * handled separately via `hasQuery()`):
   *   folder selected → folder-filtered;
   *   else tag selected → tag-filtered;
   *   else → the full list.
   * No existing branch is removed — the folder branch sits ABOVE the tag/all
   * branches the screen already had.
   */
  readonly displayedMeetings = computed(() => {
    if (this.activeFolderId() !== null) {
      return this.folderMeetings();
    }
    return this.activeTag() === null ? this.meetings() : this.tagMeetings();
  });
  /** Loading state for the visible no-query list (initial load or a tag fetch). */
  readonly listLoading = computed(() => {
    if (this.activeFolderId() !== null) {
      // Folder filtering is client-side over `meetings`, so it shares the
      // initial-load flag (and the tree's own loading shows in the left pane).
      return this.loading();
    }
    return this.activeTag() === null ? this.loading() : this.tagLoading();
  });

  /** Heading for the no-query list: folder name → tag → "Meetings". */
  readonly listHeading = computed(() => {
    const fid = this.activeFolderId();
    if (fid !== null) {
      return this.folderById().get(fid)?.name ?? "Folder";
    }
    return this.activeTag() ?? "Meetings";
  });

  /** Exposure of the active folder (for the header lock badge); null when none. */
  readonly activeFolderExposure = computed<FolderExposure | null>(() => {
    const fid = this.activeFolderId();
    if (fid === null) {
      return null;
    }
    const node = this.folderById().get(fid);
    return node ? this.folders.exposureOf(node) : null;
  });

  // --- Delete affordance (in-app, signal-driven confirm) ------------------
  /** Id of the meeting whose inline confirm panel is open (null = none). */
  readonly pendingDeleteId = signal<string | null>(null);
  /** True while a delete IPC call is in flight — guards the confirm button. */
  readonly deleting = signal(false);
  /** Non-empty when the last delete failed (cleared on the next attempt). */
  readonly deleteError = signal<string | null>(null);

  // --- Search state -------------------------------------------------------
  /** Raw, untrimmed query bound to the input. */
  readonly query = signal("");
  /** Latest applied search hits. */
  readonly results = signal<SearchHit[]>([]);
  /** True while an IPC search is in flight (drives the "Searching…" state). */
  readonly searching = signal(false);

  /** Whether the (trimmed) query is non-empty — switches list ↔ results. */
  readonly hasQuery = computed(() => this.query().trim().length > 0);

  /** Tracked so we can cancel a pending debounce on re-trigger / destroy. */
  private searchTimer: ReturnType<typeof setTimeout> | null = null;

  async ngOnInit(): Promise<void> {
    // Clean up any in-flight debounce timer when the view is torn down.
    this.destroyRef.onDestroy(() => {
      if (this.searchTimer) {
        clearTimeout(this.searchTimer);
      }
    });

    // Load the meetings list and the tag set in parallel; a tag-load failure
    // must not break the list, so settle each independently.
    const [meetings] = await Promise.allSettled([
      this.ipc.listMeetings(),
      this.loadTags(),
    ]);
    if (meetings.status === "fulfilled") {
      this.meetings.set(meetings.value);
    }
    this.loading.set(false);
  }

  /** Fetch the distinct tag set; on failure leave `tags` empty (no filter bar). */
  private async loadTags(): Promise<void> {
    try {
      this.tags.set(await this.ipc.listAllTags());
    } catch {
      this.tags.set([]);
    }
  }

  // --- Tag filtering -------------------------------------------------------

  /**
   * Select a tag (or `null` for "All"). "All" clears back to the full meetings
   * list; a tag loads its meetings into `tagMeetings`. Latest-tag-wins so a
   * slower earlier fetch can't clobber a newer selection.
   */
  async selectTag(tag: string | null): Promise<void> {
    if (this.activeTag() === tag) {
      return;
    }
    // Switching the view dismisses any open delete confirm to avoid a dangling
    // panel pointing at a row that may not be in the new list.
    this.cancelDelete();
    // Tag + folder filters are mutually exclusive: picking a tag clears any
    // active folder selection so the two never compose into an empty surprise.
    if (tag !== null) {
      this.activeFolderId.set(null);
    }
    this.activeTag.set(tag);

    if (tag === null) {
      this.tagMeetings.set([]);
      this.tagLoading.set(false);
      return;
    }

    this.tagLoading.set(true);
    try {
      const list = await this.ipc.listMeetingsByTag(tag);
      if (this.activeTag() !== tag) {
        return; // stale — a newer tag selection superseded this request.
      }
      this.tagMeetings.set(list);
    } catch {
      if (this.activeTag() === tag) {
        this.tagMeetings.set([]);
      }
    } finally {
      if (this.activeTag() === tag) {
        this.tagLoading.set(false);
      }
    }
  }

  // --- Folder filtering (left pane) ---------------------------------------

  /**
   * Select a folder (or `null` for "All notes" / the vault root). Mirrors the
   * tag-filter machinery: it dismisses any open delete confirm, clears the
   * mutually-exclusive tag filter, and (for a non-null folder) leaves the search
   * alone — the right pane re-derives `folderMeetings` reactively. A null target
   * (the tree's "All notes" row) returns to the full list. There is no async
   * fetch (folder filtering is client-side over `meetings`), so no latest-wins
   * race exists; the same idempotent-guard shape is kept for consistency.
   */
  selectFolder(folderId: string | null): void {
    if (this.activeFolderId() === folderId) {
      return;
    }
    this.cancelDelete();
    // Folder + tag filters are mutually exclusive — picking a folder clears the
    // tag selection (and its fetched list) so they never compose.
    if (folderId !== null) {
      this.activeTag.set(null);
      this.tagMeetings.set([]);
      this.tagLoading.set(false);
    }
    this.activeFolderId.set(folderId);
  }

  // --- Filing: the per-row folder chip + "Move to…" popover ----------------

  /**
   * The display name of a meeting's current folder, or null when it's at the
   * vault root. Drives the folder chip's label ("Marketing" vs "+ Add to folder").
   */
  folderNameOf(m: Meeting): string | null {
    const fid = m.folderId ?? null;
    if (fid === null) {
      return null;
    }
    return this.folderById().get(fid)?.name ?? null;
  }

  /** Open / close / toggle the row's folder picker popover (one open at a time). */
  toggleMovePopover(id: string): void {
    this.movePopoverId.update((cur) => (cur === id ? null : id));
  }
  closeMovePopover(): void {
    this.movePopoverId.set(null);
  }

  /** Re-seal every session-unlocked folder at once (privacy "panic" affordance). */
  async relockAll(): Promise<void> {
    if (this.relockingAll()) {
      return;
    }
    this.relockingAll.set(true);
    try {
      await this.folders.relockAll();
      this.toast.success("All folders re-sealed");
    } catch {
      this.toast.danger("Couldn’t re-seal folders. Please try again.");
    } finally {
      this.relockingAll.set(false);
    }
  }

  /**
   * Apply a move (from the row chip popover OR a drag-drop) into `folderId` (null
   * = vault root). The FoldersService reloads the tree (so counts refresh), and
   * we patch the LIBRARY-LOCAL `meetings` signal's `folderId` so the derived
   * `folderMeetings` recomputes at once — the moved note leaves the current
   * folder view without a manual reload. `tagMeetings` is patched in lockstep so
   * a tag view stays coherent if one is active.
   */
  private async applyMove(
    meetingId: string,
    folderId: string | null,
  ): Promise<void> {
    const patch = (list: Meeting[]): Meeting[] =>
      list.map((m) => (m.id === meetingId ? { ...m, folderId } : m));
    this.meetings.update(patch);
    this.tagMeetings.update(patch);
  }

  /**
   * The popover's `moved` output already ran the IPC move via FoldersService;
   * here we only reconcile the local list + close the popover.
   */
  onMoved(meetingId: string, folderId: string | null): void {
    void this.applyMove(meetingId, folderId);
    this.closeMovePopover();
  }

  // --- Filing: drag a row onto a folder (the enhancement path) -------------

  /** Begin a row drag: stash the meeting id on the transfer + the shared signal. */
  onRowDragStart(event: DragEvent, m: Meeting): void {
    // A locked-and-not-unlocked note is masked; dragging it is still fine (the
    // move runs through the same load-bearing confirm at the destination), so we
    // allow it. The transfer carries the id under our private MIME type.
    if (event.dataTransfer) {
      event.dataTransfer.effectAllowed = "move";
      event.dataTransfer.setData(NoteDragService.MIME, m.id);
    }
    this.drag.begin(m.id);
  }

  /** End a row drag (fires whether or not it landed on a target). */
  onRowDragEnd(): void {
    this.drag.end();
  }

  /**
   * A note was dropped onto a folder (or "All notes"). Run the move through the
   * FoldersService (which owns the cross-encryption-boundary semantics + tree
   * reload), then reconcile the local list. A no-op when it's already there.
   */
  async onDropNote(payload: {
    meetingId: string;
    folderId: string | null;
  }): Promise<void> {
    const { meetingId, folderId } = payload;
    const current =
      this.meetings().find((m) => m.id === meetingId)?.folderId ?? null;
    if (current === folderId) {
      return; // already filed here — nothing to do.
    }
    try {
      await this.folders.moveNote(meetingId, folderId);
      await this.applyMove(meetingId, folderId);
      const name =
        folderId === null
          ? "All notes"
          : (this.folderById().get(folderId)?.name ?? "folder");
      this.toast.success(`Moved to ${name}`);
    } catch {
      this.toast.danger("Couldn’t move this note. Please try again.");
    }
  }

  // --- Lock-aware row rendering -------------------------------------------

  /**
   * The exposure of the folder a meeting lives in (open / locked / session), or
   * null when the note is at the vault root / its folder isn't known. Drives the
   * inline lock badge on a meeting row.
   */
  folderExposureOf(m: Meeting): FolderExposure | null {
    const fid = m.folderId ?? null;
    if (fid === null) {
      return null;
    }
    const node = this.folderById().get(fid);
    return node ? this.folders.exposureOf(node) : null;
  }

  /**
   * Whether a meeting's title must be masked: it lives in a folder that is
   * sealed and NOT session-unlocked (`exposure === 'locked'`). A session-
   * unlocked folder ('session') shows its titles normally.
   */
  isMasked(m: Meeting): boolean {
    return this.folderExposureOf(m) === "locked";
  }

  // --- Search-as-you-type --------------------------------------------------

  /** Mirror the input into the `query` signal, then debounce the search. */
  onQueryInput(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
    this.scheduleSearch();
  }

  /**
   * Debounced search dispatch (DestroyRef-tracked timeout — no bare setTimeout
   * lifecycle). An empty/whitespace query clears results immediately; a real
   * query runs after the debounce window.
   */
  private scheduleSearch(): void {
    if (this.searchTimer) {
      clearTimeout(this.searchTimer);
      this.searchTimer = null;
    }

    const q = this.query().trim();
    if (!q) {
      // Empty query: drop any in-flight state and show the meetings list.
      this.searching.set(false);
      this.results.set([]);
      return;
    }

    // Search takes precedence over BOTH the tag and folder filters: reset to
    // the full list so clearing the search returns to "All" (the chip bar is
    // hidden while searching). Per-row delete still works against `meetings`.
    if (this.activeTag() !== null) {
      void this.selectTag(null);
    }
    if (this.activeFolderId() !== null) {
      this.activeFolderId.set(null);
    }

    this.searching.set(true);
    this.searchTimer = setTimeout(() => {
      void this.runSearch(q);
    }, SEARCH_DEBOUNCE_MS);
  }

  /**
   * Execute one search. Latest-query-wins: by the time the promise resolves the
   * user may have typed on, so we only apply results if `q` still matches the
   * current trimmed query — otherwise a slower earlier request can't clobber a
   * newer one.
   */
  private async runSearch(q: string): Promise<void> {
    try {
      const hits = await this.ipc.searchMeetings(q);
      if (this.query().trim() !== q) {
        return; // stale — a newer keystroke superseded this request.
      }
      this.results.set(hits);
    } catch {
      if (this.query().trim() === q) {
        this.results.set([]);
      }
    } finally {
      if (this.query().trim() === q) {
        this.searching.set(false);
      }
    }
  }

  /** Reset the query + results and return focus to the input. */
  clear(): void {
    if (this.searchTimer) {
      clearTimeout(this.searchTimer);
      this.searchTimer = null;
    }
    this.query.set("");
    this.results.set([]);
    this.searching.set(false);
    this.searchInput()?.nativeElement.focus();
  }

  // --- Delete a meeting (open confirm → await IPC → prune signal) ----------

  /**
   * Open the inline confirm panel for `id`. The triggering ✕ button calls
   * `preventDefault`/`stopPropagation` itself so the row never navigates.
   */
  askDelete(id: string): void {
    this.deleteError.set(null);
    this.pendingDeleteId.set(id);
  }

  /** Dismiss the confirm panel without deleting (ignored mid-flight). */
  cancelDelete(): void {
    if (this.deleting()) {
      return;
    }
    this.pendingDeleteId.set(null);
    this.deleteError.set(null);
  }

  /**
   * Confirm the pending delete: await the irreversible IPC call, then prune the
   * row from the local `meetings` signal (no full reload needed). On failure we
   * surface an inline error and keep the panel open so the user can retry.
   */
  async confirmDelete(id: string): Promise<void> {
    if (this.deleting()) {
      return;
    }
    this.deleting.set(true);
    this.deleteError.set(null);
    try {
      await this.ipc.deleteMeeting(id);
      // Prune from both lists so whichever view is showing updates at once.
      this.meetings.update((list) => list.filter((m) => m.id !== id));
      this.tagMeetings.update((list) => list.filter((m) => m.id !== id));
      this.pendingDeleteId.set(null);
    } catch {
      this.deleteError.set("Couldn’t delete this meeting. Please try again.");
    } finally {
      this.deleting.set(false);
    }
  }

  // --- Snippet highlighting (no innerHTML / DomSanitizer) ------------------

  /**
   * Split a snippet into runs around case-insensitive matches of the current
   * query, so the template can wrap matched runs in a styled <mark> element.
   * Returns a single non-hit run when the query doesn't occur in the snippet.
   */
  snippetParts(snippet: string): SnippetPart[] {
    const q = this.query().trim();
    if (!q) {
      return [{ text: snippet, hit: false }];
    }
    const parts: SnippetPart[] = [];
    const haystack = snippet.toLowerCase();
    const needle = q.toLowerCase();
    let from = 0;
    let at = haystack.indexOf(needle, from);
    while (at !== -1) {
      if (at > from) {
        parts.push({ text: snippet.slice(from, at), hit: false });
      }
      parts.push({ text: snippet.slice(at, at + needle.length), hit: true });
      from = at + needle.length;
      at = haystack.indexOf(needle, from);
    }
    if (from < snippet.length) {
      parts.push({ text: snippet.slice(from), hit: false });
    }
    return parts;
  }

  /** Human label for the field a hit matched in. */
  matchLabel(matchedIn: string): string {
    switch (matchedIn) {
      case "transcript":
        return "in transcript";
      case "note":
        return "in note";
      default:
        return "title";
    }
  }

  /** Tint the matched-in badge: transcript/note = accent, title = neutral. */
  matchBadgeClass(matchedIn: string): string {
    switch (matchedIn) {
      case "transcript":
        return "is-accent";
      case "note":
        return "is-success";
      default:
        return "";
    }
  }

  statusLabel(s: string): string {
    return s.charAt(0) + s.slice(1).toLowerCase();
  }

  /** Maps a meeting status to a status-pill state modifier (matches Record). */
  statusPillClass(s: MeetingStatus): string {
    switch (s) {
      case "RECORDING":
      case "ERROR":
        return "is-danger";
      case "TRANSCRIBED":
      case "SUMMARIZED":
        return "is-accent";
      case "EXPORTED":
        return "is-success";
      default:
        return "";
    }
  }

  /** Presentational only: render the stored timestamp as a friendly local date. */
  formatDate(startedAt: string): string {
    const d = new Date(startedAt);
    if (Number.isNaN(d.getTime())) {
      return startedAt;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  /** Presentational only: seconds → compact "Hh Mm" / "Mm Ss" / "Ss" duration. */
  formatDuration(durationS: number): string {
    const total = Math.max(0, Math.round(durationS));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) {
      return `${h}h ${m}m`;
    }
    if (m > 0) {
      return `${m}m ${s}s`;
    }
    return `${s}s`;
  }
}
