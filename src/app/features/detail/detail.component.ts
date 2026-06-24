import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { ActivatedRoute, Router, RouterLink } from "@angular/router";
import { convertFileSrc } from "@tauri-apps/api/core";
import { IpcService } from "../../core/ipc.service";
import type { MeetingDetail, MeetingTimeline } from "../../core/models";
import { MeetingChatComponent } from "./meeting-chat.component";
import { MeetingTimelineComponent } from "./meeting-timeline.component";

/** One checklist entry parsed from a `- [ ]` / `- [x]` action-item line. */
interface ActionItem {
  done: boolean;
  text: string;
}

/** A parsed `## Heading` section of the note body. */
interface NoteSection {
  heading: string;
  /** Normalised kind drives which renderer the template uses. */
  kind: "actions" | "bullets" | "prose";
  /** Plain prose paragraphs (kind === 'prose'). */
  paragraphs: string[];
  /** Bullet lines, leading marker stripped (kind === 'bullets'). */
  bullets: string[];
  /** Checklist entries (kind === 'actions'). */
  actions: ActionItem[];
}

/** The whole note, decomposed into front-matter + body sections. */
interface ParsedNote {
  tags: string[];
  participants: string[];
  sections: NoteSection[];
  /** Set only when the body contained no `## ` sections — raw fallback. */
  raw: string | null;
}

@Component({
  selector: "app-detail",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, MeetingTimelineComponent, MeetingChatComponent],
  template: `
    <section class="detail">
      <a routerLink="/library" class="back">
        <span class="back-arrow" aria-hidden="true">←</span>
        <span>Meetings</span>
      </a>

      @if (detail(); as d) {
        <header class="head">
          <div class="head-text">
            @if (renaming()) {
              <div class="rename">
                <input
                  #renameInput
                  type="text"
                  class="rename-input"
                  aria-label="Meeting title"
                  autocapitalize="sentences"
                  autocomplete="off"
                  [value]="titleDraft()"
                  [disabled]="savingRename()"
                  (input)="onTitleInput($event)"
                  (keydown.enter)="saveRename()"
                  (keydown.escape)="cancelRename()"
                />
                <div class="rename-actions">
                  <button
                    type="button"
                    class="btn btn-ghost rename-btn"
                    (click)="cancelRename()"
                    [disabled]="savingRename()"
                  >
                    Cancel
                  </button>
                  <button
                    type="button"
                    class="btn btn-primary rename-btn"
                    (click)="saveRename()"
                    [disabled]="savingRename() || !titleDraft().trim()"
                  >
                    {{ savingRename() ? "Saving…" : "Save" }}
                  </button>
                </div>
              </div>
            } @else {
              <h2>{{ d.meeting.title || "(untitled)" }}</h2>
            }
            <div class="meta">
              <span class="pill" [class]="statusPillClass(d.meeting.status)">
                <span class="pill-dot"></span>
                {{ d.meeting.status }}
              </span>
              <span class="meta-sep" aria-hidden="true">·</span>
              <span class="meta-item">{{
                formatDate(d.meeting.startedAt)
              }}</span>
              <span class="meta-sep" aria-hidden="true">·</span>
              <span class="meta-item">{{
                formatDuration(d.meeting.durationS)
              }}</span>
            </div>
          </div>

          <div class="actions">
            <button
              type="button"
              class="btn btn-primary"
              (click)="resummarize(d.meeting.id)"
              [disabled]="busy() || renaming()"
            >
              Re-summarize
            </button>
            @if (!renaming()) {
              <button
                type="button"
                class="btn btn-ghost"
                (click)="startRename()"
                [disabled]="busy()"
              >
                Rename
              </button>
              <button
                type="button"
                class="btn btn-danger"
                (click)="askDelete()"
                [disabled]="busy()"
              >
                Delete
              </button>
            }
            @if (msg()) {
              <span class="msg">{{ msg() }}</span>
            }
          </div>
        </header>

        <!-- In-app delete confirmation (signal-driven; no window.confirm) ----- -->
        @if (confirmingDelete()) {
          <div
            class="card confirm"
            role="alertdialog"
            aria-label="Delete meeting"
          >
            <div class="confirm-text">
              <p class="confirm-title">Delete this meeting?</p>
              <p class="confirm-copy">
                This permanently removes the recording, transcript, summary and
                the note in your vault. This can’t be undone.
              </p>
              @if (deleteError(); as err) {
                <p class="confirm-error" role="alert">{{ err }}</p>
              }
            </div>
            <div class="confirm-actions">
              <button
                type="button"
                class="btn btn-ghost"
                (click)="cancelDelete()"
                [disabled]="deleting()"
              >
                Cancel
              </button>
              <button
                type="button"
                class="btn btn-danger"
                (click)="confirmDelete(d.meeting.id)"
                [disabled]="deleting()"
              >
                {{ deleting() ? "Deleting…" : "Delete" }}
              </button>
            </div>
          </div>
        }

        <!-- 1) AUDIO PLAYER ------------------------------------------------ -->
        @if (audioSrc(); as src) {
          <div class="card player" [style.animation-delay.ms]="40">
            <audio
              #player
              [src]="src"
              preload="metadata"
              (loadedmetadata)="onLoaded()"
              (timeupdate)="onTimeUpdate()"
              (play)="playing.set(true)"
              (pause)="playing.set(false)"
              (ended)="onEnded()"
            ></audio>

            <button
              type="button"
              class="play"
              (click)="togglePlay()"
              [attr.aria-label]="playing() ? 'Pause' : 'Play'"
              [class.is-playing]="playing()"
            >
              @if (playing()) {
                <span class="icon-pause" aria-hidden="true"></span>
              } @else {
                <span class="icon-play" aria-hidden="true"></span>
              }
            </button>

            <div class="player-body">
              <div
                class="track"
                role="slider"
                tabindex="0"
                aria-label="Seek"
                [attr.aria-valuemin]="0"
                [attr.aria-valuemax]="Math.round(duration())"
                [attr.aria-valuenow]="Math.round(currentTime())"
                (click)="seekFromEvent($event)"
                (keydown)="onTrackKey($event)"
              >
                <div class="track-fill" [style.width.%]="progressPct()">
                  <span class="track-knob"></span>
                </div>
              </div>
              <div class="times">
                <span class="time">{{ fmt(currentTime()) }}</span>
                <span class="time time-total">{{ fmt(duration()) }}</span>
              </div>
            </div>
          </div>
        } @else {
          <div class="card player player--empty">
            <span class="audio-off" aria-hidden="true"></span>
            <span class="audio-off-text">Audio not available</span>
          </div>
        }

        <!-- 1b) INTERACTIVE TIMELINE (speakers + topics, shared playhead) -- -->
        <app-meeting-timeline
          [timeline]="timeline()"
          [total]="timelineTotal()"
          [currentTime]="currentTime()"
          [loading]="timelineLoading()"
          [error]="timelineError()"
          [hasAudio]="!!audioSrc()"
          (seek)="seekTo($event)"
          (retry)="loadTimeline()"
        />

        <!-- 2) RICH ANALYSIS ---------------------------------------------- -->
        <section class="block">
          <div class="block-head">
            <h3>Analysis</h3>
            @if (!editing() && note()?.tags?.length) {
              <div class="tags">
                @for (t of note()!.tags; track t) {
                  <span class="pill tag">{{ t }}</span>
                }
              </div>
            }
            @if (note() && !editing()) {
              <button
                type="button"
                class="btn btn-ghost edit-btn"
                (click)="startEdit()"
              >
                Edit
              </button>
            }
          </div>

          @if (note(); as n) {
            @if (editing()) {
              <!-- In-app note editor (raw markdown → re-written to the vault) -->
              <article class="card editor">
                <textarea
                  class="editor-area"
                  spellcheck="false"
                  autocapitalize="off"
                  autocomplete="off"
                  aria-label="Note markdown"
                  [value]="draft()"
                  [disabled]="saving()"
                  (input)="onDraftInput($event)"
                ></textarea>

                @if (saveError(); as err) {
                  <p class="editor-error" role="alert">{{ err }}</p>
                }

                <div class="editor-foot">
                  <span class="editor-hint"
                    >Markdown · saved to your vault</span
                  >
                  <div class="editor-actions">
                    <button
                      type="button"
                      class="btn btn-ghost"
                      (click)="cancelEdit()"
                      [disabled]="saving()"
                    >
                      Cancel
                    </button>
                    <button
                      type="button"
                      class="btn btn-primary"
                      (click)="saveNote()"
                      [disabled]="saving()"
                    >
                      {{ saving() ? "Saving…" : "Save" }}
                    </button>
                  </div>
                </div>
              </article>
            } @else {
              @if (n.participants.length) {
                <div class="card meta-card" [style.animation-delay.ms]="80">
                  <span class="meta-card-label">Participants</span>
                  <div class="people">
                    @for (p of n.participants; track p) {
                      <span class="person">{{ p }}</span>
                    }
                  </div>
                </div>
              }

              @if (n.sections.length) {
                @for (sec of n.sections; track sec.heading; let i = $index) {
                  <article
                    class="card section"
                    [style.animation-delay.ms]="120 + i * 60"
                  >
                    <h4 class="section-head">{{ sec.heading }}</h4>

                    @switch (sec.kind) {
                      @case ("actions") {
                        <ul class="checklist">
                          @for (a of sec.actions; track $index) {
                            <li class="check" [class.is-done]="a.done">
                              <span
                                class="check-box"
                                [class.is-done]="a.done"
                                aria-hidden="true"
                              ></span>
                              <span class="check-text">{{ a.text }}</span>
                            </li>
                          }
                        </ul>
                      }
                      @case ("bullets") {
                        <ul class="bullets">
                          @for (b of sec.bullets; track $index) {
                            <li class="bullet">{{ b }}</li>
                          }
                        </ul>
                      }
                      @default {
                        <div class="prose">
                          @for (para of sec.paragraphs; track $index) {
                            <p>{{ para }}</p>
                          }
                        </div>
                      }
                    }
                  </article>
                }
              } @else if (n.raw) {
                <article class="card section" [style.animation-delay.ms]="120">
                  <pre class="note-body">{{ n.raw }}</pre>
                </article>
              }

              @if (justSaved()) {
                <div class="saved-toast" role="status">
                  <span class="saved-toast-check" aria-hidden="true"></span>
                  Saved
                </div>
              }

              @if (d.note?.exportedPath; as path) {
                <div class="card saved" [style.animation-delay.ms]="160">
                  <span class="saved-icon" aria-hidden="true"></span>
                  <div class="saved-body">
                    <span class="saved-label">Saved to vault</span>
                    <span class="saved-path">{{ path }}</span>
                  </div>
                  <button
                    type="button"
                    class="btn btn-ghost copy-btn"
                    (click)="copy(path)"
                  >
                    {{ copied() ? "Copied" : "Copy path" }}
                  </button>
                </div>
              }
            }
          } @else {
            <div class="card empty-card empty-state">
              <span class="empty-mark" aria-hidden="true"></span>
              <p class="empty-title">No analysis yet</p>
              <p class="empty">
                Re-summarize this meeting to generate a structured note.
              </p>
            </div>
          }
        </section>

        <!-- 2b) CHAT WITH THIS MEETING (grounded Q&A over the transcript) -- -->
        <section class="block">
          <app-meeting-chat [meetingId]="d.meeting.id" />
        </section>

        <!-- 3) CLICK-TO-SEEK TRANSCRIPT ----------------------------------- -->
        <section class="block">
          <div class="block-head">
            <h3>Transcript</h3>
            @if (d.segments.length) {
              <span class="count">{{ d.segments.length }}</span>
            }
          </div>

          @if (d.segments.length) {
            <div class="card transcript-card" [style.animation-delay.ms]="200">
              <ul class="segs">
                @for (s of d.segments; track s.idx) {
                  <li>
                    <button
                      type="button"
                      class="seg"
                      [class.is-active]="isActiveSegment(s.startS, s.endS)"
                      [disabled]="!audioSrc()"
                      (click)="seekTo(s.startS)"
                    >
                      <span class="seg-time">{{ fmt(s.startS) }}</span>
                      <span class="seg-text">{{ s.text }}</span>
                    </button>
                  </li>
                }
              </ul>
            </div>
          } @else {
            <div class="card empty-card">
              <p class="empty">No transcript.</p>
            </div>
          }
        </section>
      } @else if (loading()) {
        <div class="card state-card">
          <p class="empty">Loading…</p>
        </div>
      } @else {
        <div class="card empty-card empty-state">
          <span class="empty-mark" aria-hidden="true"></span>
          <p class="empty-title">Meeting not found</p>
          <p class="empty">It may have been deleted.</p>
        </div>
      }
    </section>
  `,
  styles: [
    `
      .detail {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        animation: rise 380ms var(--transition) both;
      }

      /* --- Back link --- */
      .back {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        align-self: flex-start;
        color: var(--text-secondary);
        font-size: 0.875rem;
        font-weight: 550;
      }
      .back:hover {
        color: var(--text-primary);
      }
      .back:focus-visible {
        outline: none;
        color: var(--text-primary);
        border-radius: var(--radius-sm);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .back-arrow {
        font-size: 1rem;
        line-height: 1;
      }

      /* --- Header: title, status + meta, primary action --- */
      .head {
        display: flex;
        flex-wrap: wrap;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-4);
      }
      .head-text {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        min-width: 0;
      }
      .head h2 {
        margin: 0;
      }
      .meta {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-muted);
        font-size: 0.8125rem;
      }
      .meta-item,
      .meta-sep {
        color: var(--text-muted);
      }

      .actions {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-3);
      }
      .msg {
        color: var(--text-secondary);
        font-size: 0.85rem;
      }

      /* --- Inline title rename --- */
      .rename {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
      }
      .rename-input {
        flex: 1 1 16rem;
        min-width: 0;
        height: 44px;
        font-size: 1.25rem;
        font-weight: 600;
        letter-spacing: -0.025em;
      }
      .rename-actions {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .rename-btn {
        height: 36px;
        padding: 0 var(--space-3);
        font-size: 0.875rem;
      }

      /* --- In-app delete confirmation --- */
      .confirm {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        border-color: rgba(255, 107, 107, 0.3);
        animation: rise 240ms var(--transition) both;
      }
      .confirm-text {
        min-width: 0;
        flex: 1 1 20rem;
      }
      .confirm-title {
        margin: 0 0 var(--space-1);
        color: var(--text-primary);
        font-weight: 600;
      }
      .confirm-copy {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.875rem;
        line-height: 1.55;
      }
      .confirm-error {
        margin: var(--space-2) 0 0;
        color: var(--danger);
        font-size: 0.85rem;
      }
      .confirm-actions,
      .editor-actions {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin-left: auto;
      }

      /* --- Section blocks --- */
      .block {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .block-head {
        display: flex;
        align-items: center;
        gap: var(--space-3);
      }
      .block-head h3 {
        margin: 0;
      }

      /* ========================================================== */
      /* 1) Audio player                                            */
      /* ========================================================== */
      .player {
        display: flex;
        align-items: center;
        gap: var(--space-4);
        padding: var(--space-4) var(--space-5);
        animation: rise 420ms var(--transition) both;
      }
      .player--empty {
        justify-content: flex-start;
        gap: var(--space-3);
        color: var(--text-muted);
      }
      .audio-off {
        width: 10px;
        height: 10px;
        border-radius: 50%;
        background: var(--text-muted);
        opacity: 0.6;
        flex: none;
      }
      .audio-off-text {
        color: var(--text-muted);
        font-size: 0.875rem;
      }

      /* Big accent play/pause */
      .play {
        flex: none;
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 56px;
        height: 56px;
        border: none;
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
        color: var(--text-on-accent);
        cursor: pointer;
        box-shadow: var(--shadow-accent), var(--glass-highlight);
        transition:
          transform var(--transition-fast),
          filter var(--transition),
          box-shadow var(--transition);
      }
      .play:hover {
        filter: brightness(1.08);
        transform: translateY(-1px);
      }
      .play:active {
        transform: translateY(0) scale(0.96);
      }
      .play:focus-visible {
        outline: none;
        box-shadow:
          0 0 0 3px var(--accent-ring),
          var(--shadow-accent);
      }
      .play.is-playing {
        box-shadow:
          0 0 0 1px var(--accent-ring),
          0 10px 34px rgba(110, 118, 255, 0.5);
      }
      /* Pure-CSS glyphs (no icon dependency) */
      .icon-play {
        width: 0;
        height: 0;
        margin-left: 3px;
        border-style: solid;
        border-width: 9px 0 9px 15px;
        border-color: transparent transparent transparent currentColor;
      }
      .icon-pause {
        width: 14px;
        height: 16px;
        border-left: 5px solid currentColor;
        border-right: 5px solid currentColor;
        box-sizing: content-box;
      }

      .player-body {
        flex: 1 1 auto;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        min-width: 0;
      }

      /* Clickable seek/progress bar */
      .track {
        position: relative;
        height: 8px;
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        cursor: pointer;
        transition: height var(--transition);
      }
      .track:hover,
      .track:focus-visible {
        height: 10px;
        outline: none;
      }
      .track:focus-visible {
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .track-fill {
        position: absolute;
        inset: 0 auto 0 0;
        height: 100%;
        min-width: 2px;
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
      }
      .track-knob {
        position: absolute;
        right: 0;
        top: 50%;
        width: 14px;
        height: 14px;
        transform: translate(50%, -50%);
        border-radius: 50%;
        background: var(--text-on-accent);
        box-shadow: var(--shadow-sm);
        opacity: 0;
        transition:
          opacity var(--transition),
          transform var(--transition-fast);
      }
      .track:hover .track-knob,
      .track:focus-visible .track-knob {
        opacity: 1;
      }

      .times {
        display: flex;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .time {
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.8125rem;
        font-variant-numeric: tabular-nums;
        letter-spacing: -0.01em;
      }
      .time-total {
        color: var(--text-muted);
      }

      /* ========================================================== */
      /* 2) Rich analysis                                           */
      /* ========================================================== */
      .tags,
      .people {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2);
      }
      .tag {
        height: 24px;
        padding: var(--space-1) var(--space-3);
        background: var(--accent-soft);
        border-color: transparent;
        color: var(--accent-hover);
        font-size: 0.75rem;
        font-weight: 600;
      }

      .meta-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding: var(--space-4) var(--space-5);
        animation: rise 420ms var(--transition) both;
      }
      .meta-card-label {
        color: var(--text-muted);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.04em;
        text-transform: uppercase;
      }
      .person {
        display: inline-flex;
        align-items: center;
        padding: var(--space-1) var(--space-3);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-size: 0.8125rem;
        font-weight: 550;
      }

      .section {
        padding: var(--space-5);
        animation: rise 420ms var(--transition) both;
        transition:
          transform var(--transition),
          border-color var(--transition);
      }
      .section:hover {
        border-color: var(--border-strong);
      }
      .section-head {
        margin: 0 0 var(--space-3);
        color: var(--text-primary);
      }

      .prose p {
        margin: 0 0 var(--space-3);
        color: var(--text-secondary);
        line-height: 1.7;
        max-width: 68ch;
      }
      .prose p:last-child {
        margin-bottom: 0;
      }

      .bullets {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .bullet {
        position: relative;
        padding-left: var(--space-5);
        color: var(--text-secondary);
        line-height: 1.6;
      }
      .bullet::before {
        content: "";
        position: absolute;
        left: 4px;
        top: 0.62em;
        width: 6px;
        height: 6px;
        border-radius: 50%;
        background: var(--accent);
      }

      /* Read-only action-item checklist */
      .checklist {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .check {
        display: flex;
        align-items: flex-start;
        gap: var(--space-3);
        line-height: 1.5;
      }
      .check-box {
        flex: none;
        position: relative;
        width: 20px;
        height: 20px;
        margin-top: 0.05em;
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-sm);
        background: var(--surface-input);
      }
      .check-box.is-done {
        background: var(--accent-gradient);
        border-color: transparent;
      }
      .check-box.is-done::after {
        content: "";
        position: absolute;
        left: 6px;
        top: 2px;
        width: 5px;
        height: 10px;
        border: solid var(--text-on-accent);
        border-width: 0 2px 2px 0;
        transform: rotate(45deg);
      }
      .check-text,
      .seg-text {
        color: var(--text-secondary);
        min-width: 0;
      }
      .check.is-done .check-text {
        color: var(--text-muted);
        text-decoration: line-through;
        text-decoration-color: var(--text-muted);
      }

      /* Raw-markdown fallback */
      .note-body {
        margin: 0;
        white-space: pre-wrap;
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        max-height: 420px;
        overflow: auto;
        font-size: 0.9rem;
        line-height: 1.7;
      }

      /* Saved-to-vault line */
      .saved {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-3) var(--space-4);
        animation: rise 420ms var(--transition) both;
      }
      .saved-icon {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: var(--success);
        box-shadow: 0 0 0 4px var(--success-soft);
      }
      .saved-body {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
        flex: 1 1 auto;
      }
      .saved-label {
        color: var(--text-secondary);
        font-size: 0.8125rem;
        font-weight: 600;
      }
      .saved-path {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.75rem;
        word-break: break-all;
      }
      .copy-btn {
        flex: none;
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* Edit affordance in the Analysis header */
      .edit-btn {
        margin-left: auto;
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* In-app note editor (glassmorphism, full-height monospace textarea) */
      .editor {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-4);
        animation: rise 420ms var(--transition) both;
      }
      .editor-area {
        width: 100%;
        min-height: 360px;
        flex: 1 1 auto;
        padding: var(--space-4);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.875rem;
        font-variant-numeric: tabular-nums;
        line-height: 1.7;
        resize: vertical;
        tab-size: 2;
      }
      .editor-error {
        margin: 0;
        padding: var(--space-2) var(--space-3);
        border: 1px solid rgba(255, 107, 107, 0.3);
        border-radius: var(--radius-sm);
        background: var(--danger-soft);
        color: var(--text-primary);
        font-size: 0.85rem;
      }
      .editor-foot {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .editor-hint {
        color: var(--text-muted);
        font-size: 0.8125rem;
      }

      /* Transient "Saved" confirmation */
      .saved-toast {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        align-self: flex-start;
        padding: var(--space-1) var(--space-3);
        height: 28px;
        border-radius: var(--radius-pill);
        background: var(--success-soft);
        border: 1px solid transparent;
        color: var(--success);
        font-size: 0.8125rem;
        font-weight: 600;
        animation: rise 280ms var(--transition) both;
      }
      .saved-toast-check {
        position: relative;
        width: 14px;
        height: 14px;
        flex: none;
      }
      .saved-toast-check::after {
        content: "";
        position: absolute;
        left: 4px;
        top: 0;
        width: 4px;
        height: 9px;
        border: solid currentColor;
        border-width: 0 2px 2px 0;
        transform: rotate(45deg);
      }

      /* ========================================================== */
      /* 3) Transcript                                              */
      /* ========================================================== */
      .transcript-card {
        padding: var(--space-2);
        max-height: 480px;
        overflow: auto;
        animation: rise 420ms var(--transition) both;
      }
      .segs {
        list-style: none;
        padding: 0;
        margin: 0;
      }
      .segs li + li {
        border-top: 1px solid var(--border-subtle);
      }
      .seg {
        display: flex;
        gap: var(--space-3);
        width: 100%;
        padding: var(--space-3);
        border: none;
        border-radius: var(--radius-md);
        background: transparent;
        color: inherit;
        font: inherit;
        text-align: left;
        cursor: pointer;
        line-height: 1.6;
        transition:
          background var(--transition),
          transform var(--transition-fast);
      }
      .seg:hover:not(:disabled) {
        background: var(--surface-hover);
      }
      .seg:active:not(:disabled) {
        transform: translateY(1px);
      }
      .seg:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .seg:disabled {
        cursor: default;
      }
      .seg.is-active {
        background: var(--accent-soft);
      }
      .seg.is-active .seg-text {
        color: var(--text-primary);
      }
      .seg.is-active .seg-time {
        color: var(--accent-hover);
      }
      .seg-time {
        flex: none;
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.8125rem;
        font-variant-numeric: tabular-nums;
        padding-top: 0.1em;
      }

      /* --- Empty / loading wells (.count/.state-card/.empty* are global) --- */
      .empty-card {
        padding: var(--space-5);
      }

      @media (max-width: 720px) {
        .player {
          flex-wrap: wrap;
        }
      }
    `,
  ],
})
export class DetailComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

  /** Exposed so the template can format aria values. */
  protected readonly Math = Math;

  readonly detail = signal<MeetingDetail | null>(null);
  readonly loading = signal(true);
  readonly busy = signal(false);
  readonly msg = signal("");

  // --- Inline title rename state ------------------------------------------
  /** True while the header title is swapped for an inline text input. */
  readonly renaming = signal(false);
  /** Working copy of the title (input (input) → signal); empty values ignored. */
  readonly titleDraft = signal("");
  /** Disables Save/Cancel while a renameMeeting IPC call is in flight. */
  readonly savingRename = signal(false);
  /** Focusable rename input — focused after it renders (afterNextRender). */
  private readonly renameInput =
    viewChild<ElementRef<HTMLInputElement>>("renameInput");

  // --- In-app delete confirmation state -----------------------------------
  /** True while the signal-driven delete-confirm panel is shown. */
  readonly confirmingDelete = signal(false);
  /** True while a deleteMeeting IPC call is in flight (irreversible). */
  readonly deleting = signal(false);
  /** Inline error surfaced when the delete fails. */
  readonly deleteError = signal("");

  // --- In-app note editor state -------------------------------------------
  /** True while the raw-markdown editor replaces the rendered analysis cards. */
  readonly editing = signal(false);
  /** Two-way working copy of the note's markdown (textarea (input) → signal). */
  readonly draft = signal("");
  /** Disables Save/Cancel while an updateNote IPC call is in flight. */
  readonly saving = signal(false);
  /** Inline error surfaced when a save fails. */
  readonly saveError = signal("");
  /** Drives the brief "Saved" confirmation badge after a successful write. */
  readonly justSaved = signal(false);

  /** Tracked so we can cancel the pending "Saved" reset on destroy (no leaks). */
  private savedResetTimer: ReturnType<typeof setTimeout> | null = null;

  // --- Audio player state (driven by the <audio> event bindings) ----------
  private readonly audio = viewChild<ElementRef<HTMLAudioElement>>("player");
  readonly currentTime = signal(0);
  readonly duration = signal(0);
  readonly playing = signal(false);
  readonly copied = signal(false);

  /** Asset-protocol URL for the recording, or null when there is no audio. */
  readonly audioSrc = computed(() => {
    const path = this.detail()?.meeting.audioPath;
    return path ? convertFileSrc(path) : null;
  });

  /** Progress as a 0–100 percentage for the seek-bar fill. */
  readonly progressPct = computed(() => {
    const dur = this.duration();
    if (dur <= 0) {
      return 0;
    }
    return Math.min(100, (this.currentTime() / dur) * 100);
  });

  /** The note's markdown decomposed into front-matter + body sections. */
  readonly note = computed<ParsedNote | null>(() => {
    const md = this.detail()?.note?.markdown;
    return md ? this.parseNote(md) : null;
  });

  // --- Interactive timeline (speaker + topic viz) -------------------------
  readonly timeline = signal<MeetingTimeline | null>(null);
  readonly timelineLoading = signal(false);
  readonly timelineError = signal(false);

  /**
   * Total length for the shared timeline scale: the meeting duration, falling
   * back to the furthest end across speakers / topics / transcript segments.
   */
  readonly timelineTotal = computed(() => {
    const dur = this.detail()?.meeting.durationS ?? 0;
    if (dur > 0) {
      return dur;
    }
    let max = 0;
    const tl = this.timeline();
    for (const s of tl?.speakers ?? []) {
      max = Math.max(max, s.endS);
    }
    for (const t of tl?.topics ?? []) {
      max = Math.max(max, t.endS);
    }
    for (const seg of this.detail()?.segments ?? []) {
      max = Math.max(max, seg.endS);
    }
    return max;
  });

  async ngOnInit(): Promise<void> {
    const id = this.route.snapshot.paramMap.get("id");
    if (!id) {
      this.loading.set(false);
      return;
    }
    try {
      this.detail.set(await this.ipc.getMeetingDetail(id));
    } finally {
      this.loading.set(false);
    }
    // Kick the timeline off after the detail load; never blocks the page and
    // tolerates the first-call LLM latency (backend caches the result).
    if (this.detail()) {
      void this.loadTimeline();
    }
  }

  /** Fetch (or re-fetch, via Retry) the AI-derived speaker + topic timeline. */
  async loadTimeline(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id) {
      return;
    }
    this.timelineError.set(false);
    this.timelineLoading.set(true);
    try {
      this.timeline.set(await this.ipc.getTimeline(id));
    } catch {
      this.timeline.set(null);
      this.timelineError.set(true);
    } finally {
      this.timelineLoading.set(false);
    }
  }

  async resummarize(id: string): Promise<void> {
    this.busy.set(true);
    this.msg.set("Re-summarizing…");
    try {
      await this.ipc.resummarize(id);
      this.detail.set(await this.ipc.getMeetingDetail(id));
      this.msg.set("Done.");
    } catch (e) {
      this.msg.set("Error: " + String(e));
    } finally {
      this.busy.set(false);
    }
  }

  // --- Inline title rename -------------------------------------------------

  /** Enter rename mode, seeding the draft with the meeting's current title. */
  startRename(): void {
    this.titleDraft.set(this.detail()?.meeting.title ?? "");
    this.renaming.set(true);
    // Focus the field once it has rendered (zoneless-safe; no setTimeout).
    afterNextRender(() => this.renameInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  /** Mirror the rename input value into the `titleDraft` signal. */
  onTitleInput(event: Event): void {
    this.titleDraft.set((event.target as HTMLInputElement).value);
  }

  /** Leave rename mode without persisting. */
  cancelRename(): void {
    this.renaming.set(false);
  }

  /**
   * Persist the new title: ignore empty/whitespace values, await the rename
   * IPC, then fold the trimmed title into the in-memory meeting so the header
   * reflects it immediately. The rest of the page state is untouched.
   */
  async saveRename(): Promise<void> {
    const current = this.detail();
    const id = current?.meeting.id;
    const title = this.titleDraft().trim();
    if (!current || !id || !title) {
      return;
    }
    this.savingRename.set(true);
    try {
      await this.ipc.renameMeeting(id, title);
      this.detail.set({
        ...current,
        meeting: { ...current.meeting, title },
      });
      this.renaming.set(false);
    } catch (e) {
      this.msg.set("Couldn’t rename: " + String(e));
    } finally {
      this.savingRename.set(false);
    }
  }

  // --- In-app delete -------------------------------------------------------

  /** Open the signal-driven confirm panel (no window.confirm). */
  askDelete(): void {
    this.deleteError.set("");
    this.confirmingDelete.set(true);
  }

  /** Dismiss the confirm panel without deleting. */
  cancelDelete(): void {
    this.confirmingDelete.set(false);
  }

  /**
   * Irreversibly delete the meeting (recording, transcript, summary + the
   * exported vault note), then navigate back to the library. Errors surface
   * inline in the confirm panel and keep the user on the page.
   */
  async confirmDelete(id: string): Promise<void> {
    this.deleting.set(true);
    this.deleteError.set("");
    try {
      await this.ipc.deleteMeeting(id);
      await this.router.navigateByUrl("/library");
    } catch (e) {
      this.deleteError.set("Couldn’t delete: " + String(e));
      this.deleting.set(false);
    }
  }

  // --- In-app note editor --------------------------------------------------

  /** Enter edit mode, seeding the draft with the note's current raw markdown. */
  startEdit(): void {
    this.draft.set(this.detail()?.note?.markdown ?? "");
    this.saveError.set("");
    this.editing.set(true);
  }

  /** Two-way bind: mirror the textarea value into the `draft` signal. */
  onDraftInput(event: Event): void {
    this.draft.set((event.target as HTMLTextAreaElement).value);
  }

  /** Discard the working copy and leave edit mode unchanged. */
  cancelEdit(): void {
    this.editing.set(false);
    this.saveError.set("");
  }

  /**
   * Persist the draft: re-write the vault file via `updateNote`, fold the
   * returned markdown back into the in-memory detail signal (so the `note()`
   * computed re-parses and the analysis cards re-render), exit edit mode, then
   * flash a brief "Saved" confirmation. Errors surface inline; the page state
   * (audio / timeline / transcript) is never touched.
   */
  async saveNote(): Promise<void> {
    const meetingId = this.detail()?.meeting.id;
    if (!meetingId) {
      return;
    }
    this.saving.set(true);
    this.saveError.set("");
    try {
      const updated = await this.ipc.updateNote(meetingId, this.draft());
      const current = this.detail();
      if (current) {
        this.detail.set({ ...current, note: updated });
      }
      this.editing.set(false);
      this.flashSaved();
    } catch (e) {
      this.saveError.set("Couldn’t save: " + String(e));
    } finally {
      this.saving.set(false);
    }
  }

  /** Show the "Saved" badge for a moment (tracked timeout — cancelled on destroy). */
  private flashSaved(): void {
    this.justSaved.set(true);
    if (this.savedResetTimer) {
      clearTimeout(this.savedResetTimer);
    }
    this.savedResetTimer = setTimeout(() => this.justSaved.set(false), 2200);
    this.destroyRef.onDestroy(() => {
      if (this.savedResetTimer) {
        clearTimeout(this.savedResetTimer);
      }
    });
  }

  // --- Audio player controls ----------------------------------------------

  private get el(): HTMLAudioElement | null {
    return this.audio()?.nativeElement ?? null;
  }

  togglePlay(): void {
    const el = this.el;
    if (!el) {
      return;
    }
    if (el.paused) {
      void el.play();
    } else {
      el.pause();
    }
  }

  onLoaded(): void {
    const el = this.el;
    if (el && Number.isFinite(el.duration)) {
      this.duration.set(el.duration);
    }
  }

  onTimeUpdate(): void {
    const el = this.el;
    if (el) {
      this.currentTime.set(el.currentTime);
    }
  }

  onEnded(): void {
    this.playing.set(false);
    this.currentTime.set(this.duration());
  }

  /** Seek to a click position on the progress track. */
  seekFromEvent(event: MouseEvent): void {
    const el = this.el;
    const dur = this.duration();
    if (!el || dur <= 0) {
      return;
    }
    const bar = event.currentTarget as HTMLElement;
    const rect = bar.getBoundingClientRect();
    const ratio = Math.min(
      1,
      Math.max(0, (event.clientX - rect.left) / rect.width),
    );
    el.currentTime = ratio * dur;
    this.currentTime.set(el.currentTime);
  }

  /** Keyboard seeking on the focusable track (← / → by 5s, Home/End). */
  onTrackKey(event: KeyboardEvent): void {
    const el = this.el;
    const dur = this.duration();
    if (!el || dur <= 0) {
      return;
    }
    let next: number | null = null;
    switch (event.key) {
      case "ArrowLeft":
        next = Math.max(0, el.currentTime - 5);
        break;
      case "ArrowRight":
        next = Math.min(dur, el.currentTime + 5);
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = dur;
        break;
      case " ":
      case "Enter":
        event.preventDefault();
        this.togglePlay();
        return;
      default:
        return;
    }
    event.preventDefault();
    el.currentTime = next;
    this.currentTime.set(next);
  }

  /**
   * Click-to-seek from a transcript row or a timeline block: jump to `startS`
   * + play. With no audio element (audioPath null) we still advance the
   * `currentTime` signal so the timeline highlight + playhead respond.
   */
  seekTo(startS: number): void {
    const el = this.el;
    if (!el) {
      const total = this.timelineTotal();
      const clamped = total > 0 ? Math.min(total, Math.max(0, startS)) : startS;
      this.currentTime.set(clamped);
      return;
    }
    el.currentTime = startS;
    this.currentTime.set(startS);
    void el.play();
  }

  /** True when playback is inside [startS, endS) — highlights the live row. */
  isActiveSegment(startS: number, endS: number): boolean {
    const t = this.currentTime();
    return t >= startS && t < endS;
  }

  /** Copy a path to the clipboard (no external <a href> navigation). */
  async copy(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      this.copied.set(true);
    } catch {
      this.copied.set(false);
    }
  }

  // --- Markdown parsing ----------------------------------------------------

  /**
   * Strips a leading YAML front-matter block (between the first `---` and the
   * next `---`), pulls out `tags` + `participants`, then splits the remaining
   * body into `## ` sections. Falls back to raw markdown when no section is
   * found.
   */
  private parseNote(markdown: string): ParsedNote {
    const lines = markdown.replace(/\r\n/g, "\n").split("\n");

    let tags: string[] = [];
    let participants: string[] = [];
    let bodyStart = 0;

    // Front-matter must be the very first non-empty content.
    if (lines[0]?.trim() === "---") {
      const end = lines.findIndex((l, i) => i > 0 && l.trim() === "---");
      if (end > 0) {
        const fm = lines.slice(1, end);
        tags = this.readFrontMatterList(fm, "tags");
        participants = this.readFrontMatterList(fm, "participants");
        bodyStart = end + 1;
      }
    }

    const body = lines.slice(bodyStart);
    const sections: NoteSection[] = [];
    let current: { heading: string; lines: string[] } | null = null;

    for (const line of body) {
      const headingMatch = /^##\s+(.*)$/.exec(line);
      if (headingMatch) {
        if (current) {
          sections.push(this.buildSection(current.heading, current.lines));
        }
        current = { heading: headingMatch[1].trim(), lines: [] };
      } else if (current) {
        current.lines.push(line);
      }
    }
    if (current) {
      sections.push(this.buildSection(current.heading, current.lines));
    }

    if (sections.length === 0) {
      // No structured sections — surface the body (sans front-matter) raw.
      const raw = body.join("\n").trim();
      return { tags, participants, sections: [], raw: raw || markdown.trim() };
    }

    return { tags, participants, sections, raw: null };
  }

  /** Classify a section by its heading + content, then shape its data. */
  private buildSection(heading: string, lines: string[]): NoteSection {
    const trimmed = lines.map((l) => l.trim());

    // Action-items: lines like "- [ ] text" / "- [x] text".
    const actions: ActionItem[] = [];
    for (const l of trimmed) {
      const m = /^[-*]\s+\[( |x|X)\]\s+(.*)$/.exec(l);
      if (m) {
        actions.push({ done: m[1].toLowerCase() === "x", text: m[2].trim() });
      }
    }
    const headingIsActions = /action/i.test(heading);
    if (actions.length > 0 || headingIsActions) {
      return {
        heading,
        kind: "actions",
        paragraphs: [],
        bullets: [],
        actions,
      };
    }

    // Plain bullet list: "- text" / "* text" (strip the marker).
    const bullets: string[] = [];
    let nonBulletContent = false;
    for (const l of trimmed) {
      if (!l) {
        continue;
      }
      const m = /^[-*]\s+(.*)$/.exec(l);
      if (m) {
        bullets.push(m[1].trim());
      } else {
        nonBulletContent = true;
      }
    }
    if (bullets.length > 0 && !nonBulletContent) {
      return { heading, kind: "bullets", paragraphs: [], bullets, actions: [] };
    }

    // Otherwise prose: collapse blank-line-separated paragraphs.
    const paragraphs: string[] = [];
    let buf: string[] = [];
    const flush = (): void => {
      if (buf.length) {
        paragraphs.push(buf.join(" ").trim());
        buf = [];
      }
    };
    for (const l of trimmed) {
      if (l) {
        buf.push(l);
      } else {
        flush();
      }
    }
    flush();

    return { heading, kind: "prose", paragraphs, bullets: [], actions: [] };
  }

  /**
   * Reads a YAML list value for `key` — supports both inline
   * (`tags: [a, b]`) and block (`tags:` then `  - a`) styles.
   */
  private readFrontMatterList(fm: string[], key: string): string[] {
    const idx = fm.findIndex((l) =>
      new RegExp(`^${key}\\s*:`, "i").test(l.trim()),
    );
    if (idx === -1) {
      return [];
    }

    const line = fm[idx].trim();
    const inline = line.slice(line.indexOf(":") + 1).trim();

    if (inline) {
      // Inline list "[a, b]" or comma/space separated scalars.
      return inline
        .replace(/^\[/, "")
        .replace(/\]$/, "")
        .split(",")
        .map((s) => this.cleanScalar(s))
        .filter((s) => s.length > 0);
    }

    // Block list: subsequent "  - item" lines.
    const out: string[] = [];
    for (let i = idx + 1; i < fm.length; i++) {
      const m = /^\s*-\s+(.*)$/.exec(fm[i]);
      if (!m) {
        break;
      }
      const v = this.cleanScalar(m[1]);
      if (v) {
        out.push(v);
      }
    }
    return out;
  }

  /** Strip surrounding quotes/whitespace from a YAML scalar. */
  private cleanScalar(s: string): string {
    return s.trim().replace(/^["']/, "").replace(/["']$/, "").trim();
  }

  /** Seconds → m:ss for timestamps + player times. */
  fmt(s: number): string {
    const total = Math.max(0, Math.floor(s || 0));
    const m = Math.floor(total / 60);
    const sec = total % 60;
    return `${m}:${sec.toString().padStart(2, "0")}`;
  }

  /** Maps a meeting status to a status-pill state modifier (presentation only). */
  statusPillClass(status: string): string {
    switch (status) {
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

  /** Presentational: stored timestamp → friendly local date. */
  formatDate(startedAt: string): string {
    const d = new Date(startedAt);
    if (Number.isNaN(d.getTime())) return startedAt;
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  /** Presentational: seconds → compact "Hh Mm" / "Mm Ss" / "Ss". */
  formatDuration(durationS: number): string {
    const total = Math.max(0, Math.round(durationS));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m ${s}s`;
    return `${s}s`;
  }
}
