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
import { save } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import type {
  AssistantInteraction,
  FolderNode,
  GraphPayload,
  MeetingDetail,
  MeetingTimeline,
  Segment,
} from "../../core/models";
import {
  FoldersService,
  type FolderExposure,
} from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";
import { MarkdownComponent } from "../../shared/markdown.component";
import { AssistantSourcesComponent } from "../../shared/assistant-sources.component";
import { LockBadgeComponent } from "../folders/lock-badge.component";
import { MoveToMenuComponent } from "../folders/move-to-menu.component";
import { MeetingActionsComponent } from "./meeting-actions.component";
import { MeetingChatComponent } from "./meeting-chat.component";
import { MeetingRecipesComponent } from "./meeting-recipes.component";
import { MeetingTimelineComponent } from "./meeting-timeline.component";
import { RelatedMeetingsComponent } from "./related-meetings.component";

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

/**
 * One grounding citation, parsed from the persisted `string[]` the backend
 * stores per interaction. The backend writes `[[Title]]` for a vault source and
 * a `(web)` / `(https://…)` form for a web source — we split the two so the FE
 * can render `[[vault]]` chips vs distinct "via web" links (mirroring the live
 * assistant-actions card, whose live store carries structured citations).
 */
interface ParsedCitation {
  kind: "vault" | "web";
  /** Display label (vault title, or the host/label for a web source). */
  label: string;
  /** Resolved URL for a web source; null for a vault citation. */
  url: string | null;
}

/** A persisted assistant Q&A interaction enriched with parsed citations. */
interface AssistantQa {
  /** Stable id for `@for` tracking (createdAt + index — interactions are append-only). */
  id: string;
  command: string;
  answer: string;
  citations: ParsedCitation[];
  status: string;
  sourceLabel: string | null;
  createdAt: string;
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
  imports: [
    RouterLink,
    MeetingTimelineComponent,
    MeetingActionsComponent,
    MeetingChatComponent,
    MeetingRecipesComponent,
    LockBadgeComponent,
    MoveToMenuComponent,
    MarkdownComponent,
    AssistantSourcesComponent,
    RelatedMeetingsComponent,
  ],
  template: `
    <section class="detail">
      <a routerLink="/library" class="back">
        <span class="back-arrow" aria-hidden="true">←</span>
        <span>Meetings</span>
      </a>

      @if (detail(); as d) {
        <header class="head print-keep">
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
              <!-- Read-only folder + lock badge (where this note lives). -->
              @if (folderBadge(); as fb) {
                <span class="meta-sep" aria-hidden="true">·</span>
                <span
                  class="meta-item"
                  style="display: inline-flex; align-items: center; gap: 4px"
                  [attr.title]="fb.name"
                >
                  <app-lock-badge [exposure]="fb.exposure" />
                  {{ fb.name }}
                </span>
              }
            </div>

            <!-- TAG EDITOR: chips + inline add (persists via setMeetingTags).
                 Hidden while locked — there is nothing to tag on a masked note. -->
            @if (!locked()) {
              <div class="tag-editor">
                @for (t of tags(); track t) {
                  <span class="pill tag-chip">
                    {{ t }}
                    <button
                      type="button"
                      class="tag-x"
                      [attr.aria-label]="'Remove tag ' + t"
                      [disabled]="tagsBusy()"
                      (click)="removeTag(t)"
                    >
                      ×
                    </button>
                  </span>
                }
                <input
                  type="text"
                  class="tag-input"
                  placeholder="+ Add tag"
                  aria-label="Add tag"
                  autocapitalize="off"
                  autocomplete="off"
                  [value]="tagDraft()"
                  [disabled]="tagsBusy()"
                  (input)="onTagInput($event)"
                  (keydown.enter)="addTag()"
                />
              </div>
              @if (tagsError(); as err) {
                <span class="tag-error" role="alert">{{ err }}</span>
              }
            }
          </div>

          @if (!locked()) {
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

                <!-- MOVE TO FOLDER: opens the folder picker popover. The picker
                   itself owns the load-bearing encrypt/decrypt confirm. Layout
                   is inline (no component-stylesheet rule) so it stays anchored
                   to its trigger without growing the per-component style budget. -->
                <div style="position: relative; display: inline-flex">
                  <button
                    type="button"
                    class="btn btn-ghost"
                    [attr.aria-expanded]="moveOpen()"
                    aria-haspopup="menu"
                    (click)="toggleMove()"
                    [disabled]="busy()"
                  >
                    Move to folder
                  </button>
                  @if (moveOpen()) {
                    <div
                      style="position: absolute; top: calc(100% + 8px); left: 0; z-index: 30"
                    >
                      <app-move-to-menu
                        [meetingId]="d.meeting.id"
                        [currentFolderId]="d.meeting.folderId ?? null"
                        (moved)="onMoved($event)"
                        (close)="closeMove()"
                      />
                    </div>
                  }
                </div>
              }

              <!-- EXPORT menu: copy / save note / save audio / print-to-PDF.
                 Gated on a parsed note existing; disabled while editing it. -->
              @if (note() && !renaming()) {
                <div class="export" role="group" aria-label="Export">
                  <button
                    type="button"
                    class="btn btn-ghost export-btn"
                    (click)="copyMarkdown()"
                    [disabled]="editing() || busy()"
                  >
                    {{
                      exportMsg() === "md-copied" ? "Copied" : "Copy Markdown"
                    }}
                  </button>
                  <button
                    type="button"
                    class="btn btn-ghost export-btn"
                    (click)="saveMarkdown(d.meeting.id, d.meeting.title)"
                    [disabled]="editing() || exporting()"
                  >
                    {{
                      exportMsg() === "md-saved" ? "Saved" : "Save Markdown…"
                    }}
                  </button>
                  @if (audioSrc()) {
                    <button
                      type="button"
                      class="btn btn-ghost export-btn"
                      (click)="saveAudio(d.meeting.id, d.meeting.title)"
                      [disabled]="editing() || exporting()"
                    >
                      {{
                        exportMsg() === "audio-saved" ? "Saved" : "Save audio…"
                      }}
                    </button>
                  }
                  <button
                    type="button"
                    class="btn btn-ghost export-btn"
                    (click)="saveAsPdf()"
                    [disabled]="editing()"
                  >
                    Save as PDF
                  </button>
                  <button
                    type="button"
                    class="btn btn-ghost export-btn"
                    (click)="exportCanvas(d.meeting.id)"
                    [disabled]="editing() || exportingCanvas()"
                  >
                    {{ exportingCanvas() ? "Exporting…" : "Export Canvas" }}
                  </button>
                </div>
              }

              <!-- HI-RES MASTERS: retrieve the faithful per-stream float32 WAV
                   archives. Shown only when this install keeps masters AND the
                   meeting has audio; the backend is the source of truth and fails
                   closed (Locked when sealed, "no master for that stream" when a
                   given stream wasn't archived) — both surfaced as friendly inline
                   messages, never a crash. -->
              @if (keepsMasters() && audioSrc() && !renaming()) {
                <div
                  class="export"
                  role="group"
                  aria-label="Export hi-res master"
                >
                  <button
                    type="button"
                    class="btn btn-ghost export-btn"
                    title="Save the faithful float32 mic archive (kept because high-fidelity masters is on)."
                    (click)="exportMaster('mic', d.meeting.id, d.meeting.title)"
                    [disabled]="editing() || exporting()"
                  >
                    {{
                      exportMsg() === "mic-master-saved"
                        ? "Saved"
                        : "Export master (mic)…"
                    }}
                  </button>
                  <button
                    type="button"
                    class="btn btn-ghost export-btn"
                    title="Save the faithful float32 system-audio archive (when the other side was captured)."
                    (click)="exportMaster('sys', d.meeting.id, d.meeting.title)"
                    [disabled]="editing() || exporting()"
                  >
                    {{
                      exportMsg() === "sys-master-saved"
                        ? "Saved"
                        : "Export master (system)…"
                    }}
                  </button>
                </div>
              }
              @if (canvasMsg(); as path) {
                <div class="saved-toast canvas-toast" role="status">
                  <span class="saved-toast-check" aria-hidden="true"></span>
                  Canvas saved · {{ path }}
                </div>
              }
              @if (canvasError(); as err) {
                <span class="msg msg-error" role="alert">{{ err }}</span>
              }

              <!-- CONNECT TO GRAPH: resolve people + projects into vault stubs.
                 Gated on a parsed note existing; disabled while editing it. -->
              @if (note() && !renaming()) {
                <div class="graph-connect" role="group" aria-label="Graph">
                  <button
                    type="button"
                    class="btn btn-ghost export-btn"
                    (click)="linkGraph()"
                    [disabled]="editing() || linking()"
                  >
                    {{ linking() ? "Linking…" : "Link people &amp; projects" }}
                  </button>
                </div>
              }

              @if (exportError(); as err) {
                <span class="msg msg-error" role="alert">{{ err }}</span>
              }
              @if (msg()) {
                <span class="msg">{{ msg() }}</span>
              }
            </div>
          }
        </header>

        <!-- ============================================================= -->
        <!-- PHASE 0.5 LOCK GATE — shown when the backend masked this       -->
        <!-- meeting (sealed, not-session-unlocked folder). Replaces the    -->
        <!-- note/transcript/audio/timeline/actions with a single frosted   -->
        <!-- card + a biometric Unlock action. The masked "🔒 Locked" title -->
        <!-- bar above still shows; the back-to-Meetings nav stays.         -->
        <!-- ============================================================= -->
        @if (locked()) {
          <div
            class="card empty-state"
            role="group"
            aria-labelledby="lock-gate-title"
            style="animation: rise 420ms var(--transition) both"
          >
            <span
              aria-hidden="true"
              style="display: inline-flex; align-items: center; justify-content: center; width: 64px; height: 64px; margin-bottom: var(--space-2); border-radius: var(--radius-pill); background: var(--accent-soft); color: var(--accent-hover)"
            >
              <svg viewBox="0 0 24 24" width="28" height="28" fill="none">
                <rect
                  x="4.5"
                  y="10.5"
                  width="15"
                  height="10.5"
                  rx="2.4"
                  stroke="currentColor"
                  stroke-width="1.8"
                />
                <path
                  d="M7.75 10.5V7.75a4.25 4.25 0 0 1 8.5 0V10.5"
                  stroke="currentColor"
                  stroke-width="1.8"
                  stroke-linecap="round"
                />
                <circle cx="12" cy="15.4" r="1.6" fill="currentColor" />
              </svg>
            </span>
            <p id="lock-gate-title" class="empty-title">
              This meeting is locked
            </p>
            <p class="empty" style="max-width: 42ch">
              It lives in a locked folder. Unlock to view the note, transcript
              and audio.
            </p>
            <button
              #unlockButton
              type="button"
              class="btn btn-primary"
              style="margin-top: var(--space-2)"
              (click)="unlock()"
              [disabled]="unlocking()"
            >
              {{ unlocking() ? "Unlocking…" : "🔒 Unlock (Touch ID)" }}
            </button>
          </div>
        }

        @if (!locked()) {
          <!-- Graph link result: resolved people + projects as chips + caption. -->
          @if (graphError(); as err) {
            <div class="card graph-card graph-card--error" role="alert">
              {{ err }}
            </div>
          }
          @if (graph(); as g) {
            <div class="card graph-card" role="status">
              @if (g.people.length || g.projects.length) {
                <div class="graph-groups">
                  @if (g.people.length) {
                    <div class="graph-group">
                      <span class="graph-group-label">People</span>
                      <div class="graph-pills">
                        @for (p of g.people; track p) {
                          <span class="pill tag">{{ p }}</span>
                        }
                      </div>
                    </div>
                  }
                  @if (g.projects.length) {
                    <div class="graph-group">
                      <span class="graph-group-label">Projects</span>
                      <div class="graph-pills">
                        @for (pr of g.projects; track pr) {
                          <span class="pill tag graph-pill--project">{{
                            pr
                          }}</span>
                        }
                      </div>
                    </div>
                  }
                </div>
                <p class="graph-caption">
                  Added to your Obsidian vault graph (People/ &amp; Projects/)
                </p>
              } @else {
                <p class="graph-caption">No people or projects to link yet.</p>
              }
            </div>
          }

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
                  This permanently removes the recording, transcript, summary
                  and the note in your vault. This can’t be undone.
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
            (pin)="onPin($event)"
            (renameSpeaker)="onRenameSpeaker($event)"
          />

          <!-- Pin confirmation / error (driven by the timeline's (pin) output). -->
          @if (pinMsg(); as m) {
            <div class="saved-toast pin-toast" role="status">
              <span class="pin-toast-dot" aria-hidden="true"></span>
              {{ m }}
            </div>
          }
          @if (pinError(); as err) {
            <div class="saved-toast pin-toast pin-toast--error" role="alert">
              {{ err }}
            </div>
          }

          <!-- 2) RICH ANALYSIS ---------------------------------------------- -->
          <section class="block print-keep">
            <div class="block-head">
              <h3>Analysis</h3>
              @if (!editing() && note()?.tags?.length) {
                <div class="tags">
                  @for (t of note()!.tags; track t) {
                    <span class="pill tag">{{ t }}</span>
                  }
                </div>
              }
              <!-- Phase 5: model-provenance badge — shown only when the backend
                   recorded which model produced this note. Hidden for locked meetings
                   (provenance null) and for legacy meetings without CallMeta. -->
              @if (provenanceLabel(); as prov) {
                <span
                  class="provenance-badge"
                  [attr.title]="prov.provider ? 'Provider: ' + prov.provider : null"
                  [attr.aria-label]="'Generated by ' + (prov.model || prov.provider)"
                >
                  <svg
                    viewBox="0 0 16 16"
                    width="12"
                    height="12"
                    fill="none"
                    aria-hidden="true"
                    class="provenance-icon"
                  >
                    <circle cx="8" cy="8" r="6.5" stroke="currentColor" stroke-width="1.4" />
                    <path d="M5.5 8.5 7.2 10.2 10.5 6" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
                  </svg>
                  @if (prov.model) {
                    <span>{{ prov.model }}</span>
                  }
                  @if (prov.model && prov.provider) {
                    <span class="provenance-sep" aria-hidden="true">·</span>
                  }
                  @if (prov.provider) {
                    <span>{{ prov.provider }}</span>
                  }
                </span>
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
                  <article
                    class="card section"
                    [style.animation-delay.ms]="120"
                  >
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

          <!-- 2·25) ASSISTANT Q&A (persisted in-meeting voice exchanges) ------- -->
          @if (interactions().length) {
            <section class="block print-keep">
              <div class="block-head">
                <h3>🎙 Asystent — Q&amp;A</h3>
                <span class="count">{{ interactions().length }}</span>
              </div>

              <ul class="qa-list">
                @for (q of interactions(); track q.id) {
                  <li
                    class="card qa-row"
                    [class.is-pending]="q.status === 'pending'"
                  >
                    <div class="qa-heard">
                      <span class="qa-ico" aria-hidden="true">🎙</span>
                      <span class="qa-heard-text">
                        Pytałeś:
                        <strong>{{ q.command || "…" }}</strong>
                      </span>
                      @if (qaStatusLabel(q.status)) {
                        <span class="pill" [class]="qaStatusPillClass(q.status)">
                          <span class="pill-dot"></span>
                          {{ qaStatusLabel(q.status) }}
                        </span>
                      }
                    </div>

                    @if (q.answer) {
                      <app-markdown
                        class="qa-answer"
                        [markdown]="q.answer"
                        compact
                      />
                    }

                    @if (q.citations.length) {
                      <app-assistant-sources [citations]="q.citations" />
                    }

                    @if (q.sourceLabel) {
                      <span class="qa-source text-muted">{{
                        q.sourceLabel
                      }}</span>
                    }
                  </li>
                }
              </ul>
            </section>
          }

          <!-- 2·5) ACTION ITEMS (Reminders + Obsidian Tasks; hidden when none) - -->
          <app-meeting-actions [meetingId]="d.meeting.id" />

          <!-- 2a) RECIPES / GENERATE (grounded one-tap generations over text) - -->
          <section class="block">
            <div class="block-head">
              <h3>Recipes</h3>
            </div>
            <app-meeting-recipes [meetingId]="d.meeting.id" />
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
              <div
                class="card transcript-card"
                [style.animation-delay.ms]="200"
              >
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
                        @if (speakerChip(s.speaker); as chip) {
                          <span
                            class="seg-speaker"
                            [style.background]="chip.bg"
                            [style.color]="chip.fg"
                            >{{ chip.label }}</span
                          >
                        }
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

          <!-- 4) RELATED BY MEANING (semantic neighbors; silent when empty) -- -->
          <app-related-meetings
            [meetingId]="d.meeting.id"
            (open)="openRelated($event)"
          />
        }
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
      .head-text,
      .graph-group {
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

      /* --- Tag editor (chips + inline add) --- */
      .tag-editor {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
      }
      .tag-chip {
        height: 26px;
        padding: 0 var(--space-1) 0 var(--space-3);
        background: var(--accent-soft);
        border-color: transparent;
        color: var(--accent-hover);
        font-size: 0.75rem;
        font-weight: 600;
      }
      .tag-x {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 18px;
        height: 18px;
        padding: 0;
        border: none;
        border-radius: var(--radius-pill);
        background: transparent;
        color: inherit;
        font-size: 0.95rem;
        line-height: 1;
        cursor: pointer;
        opacity: 0.7;
        transition:
          background var(--transition),
          opacity var(--transition);
      }
      .tag-x:hover:not(:disabled) {
        background: var(--surface-hover);
        opacity: 1;
      }
      .tag-x:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .tag-x:disabled {
        cursor: default;
        opacity: 0.4;
      }
      .tag-input {
        width: auto;
        flex: 0 1 9rem;
        height: 26px;
        padding: 0 var(--space-3);
        border-radius: var(--radius-pill);
        font-size: 0.75rem;
      }
      .tag-error {
        color: var(--danger);
        font-size: 0.8125rem;
      }

      /* Read-only folder + lock badge in the meta row. */
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
      .msg-error {
        color: var(--danger);
      }

      /* --- Export menu + graph-connect (ghost buttons; leading divider) --- */
      .export,
      .graph-connect {
        display: inline-flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-1);
        padding-left: var(--space-3);
        border-left: 1px solid var(--border-subtle);
      }
      .export-btn {
        height: 36px;
        padding: 0 var(--space-3);
        font-size: 0.875rem;
      }
      /* Pin toast reuses .saved-toast box (accent variant). */
      .pin-toast {
        background: var(--accent-soft);
        color: var(--accent-hover);
      }
      .pin-toast--error,
      .graph-card--error {
        background: var(--danger-soft);
        color: var(--danger);
      }
      .graph-groups {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-4) var(--space-5);
      }
      .graph-pill--project {
        background: var(--success-soft);
        color: var(--success);
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

      /* Phase 5 — model-provenance ghost chip (Analysis header). */
      .provenance-badge {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        padding: 2px var(--space-2);
        border-radius: var(--radius-pill);
        border: 1px solid var(--border-subtle);
        color: var(--text-muted);
        font-size: 0.6875rem;
        font-weight: 500;
        white-space: nowrap;
        margin-left: auto;
      }
      .provenance-icon { flex: none; opacity: 0.7; }
      .provenance-sep { opacity: 0.5; }

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
      .people,
      .graph-pills {
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

      .meta-card,
      .graph-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-4) var(--space-5);
        animation: rise 420ms var(--transition) both;
      }
      .meta-card-label,
      .graph-group-label {
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

      /* Assistant — Q&A section (mirrors the live assistant-actions card) */
      .qa-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .qa-row {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding: var(--space-3) var(--space-4);
        animation: rise 360ms var(--transition) both;
      }
      .qa-heard {
        display: flex;
        align-items: center;
        flex-wrap: wrap;
        gap: var(--space-2);
      }
      .qa-heard-text {
        color: var(--text-secondary);
        font-size: 0.875rem;
      }
      .qa-heard-text strong {
        color: var(--text-primary);
        font-weight: 600;
      }
      .qa-heard .pill {
        margin-left: auto;
      }
      /* The answer is now rendered by app-markdown; just give the block room. */
      .qa-answer {
        display: block;
        font-size: 0.9rem;
      }
      .qa-source {
        font-size: 0.72rem;
        text-transform: uppercase;
        letter-spacing: 0.03em;
      }
      @media (prefers-reduced-motion: reduce) {
        .qa-row {
          animation: none;
        }
      }

      /* Saved-to-vault line */
      .saved {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-3) var(--space-4);
        animation: rise 420ms var(--transition) both;
      }
      .saved-icon,
      .pin-toast-dot {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: var(--success);
        box-shadow: 0 0 0 4px var(--success-soft);
      }
      .pin-toast-dot {
        background: var(--accent);
        box-shadow: 0 0 0 4px var(--accent-soft);
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
      .editor-hint,
      .graph-caption {
        margin: 0;
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
        min-height: 28px;
        border-radius: var(--radius-pill);
        background: var(--success-soft);
        color: var(--success);
        font-size: 0.8125rem;
        font-weight: 600;
        animation: rise 280ms var(--transition) both;
      }
      .canvas-toast {
        word-break: break-all;
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

      /* Speaker chip: an optional Me/Others tag between the time + text
         (colours bound inline). Legacy/null segments render unlabeled. */
      .seg-speaker {
        flex: none;
        padding: 2px var(--space-2);
        border-radius: var(--radius-pill);
        font-size: 0.6875rem;
        font-weight: 700;
        line-height: 1.5;
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

      /* Print / Save-as-PDF — isolate the note + analysis. Driven by
         window.print(); the app chrome (header/nav, aurora) that lives outside
         this component is hidden globally via body.murmur-printing. */
      @media print {
        /* Hide the whole detail view, then re-reveal only title + analysis. */
        .detail > * {
          display: none !important;
        }
        .detail > .print-keep {
          display: flex !important;
        }
        /* Within the kept regions, drop the interactive affordances. */
        .head .actions,
        .head .export,
        .head .msg,
        .block .edit-btn,
        .block .saved,
        .block .saved-toast {
          display: none !important;
        }
        .head.print-keep {
          justify-content: flex-start;
        }
        /* Flatten frosted cards + force ink-friendly dark text. */
        .card,
        .section,
        .meta-card {
          background: #fff !important;
          border-color: #ccc !important;
          box-shadow: none !important;
          -webkit-backdrop-filter: none !important;
          backdrop-filter: none !important;
          break-inside: avoid;
        }
        .print-keep,
        .print-keep * {
          color: #000 !important;
          animation: none !important;
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
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);

  /** Exposed so the template can format aria values. */
  protected readonly Math = Math;

  readonly detail = signal<MeetingDetail | null>(null);
  readonly loading = signal(true);
  readonly busy = signal(false);
  readonly msg = signal("");

  // --- Phase 0.5 lock gate -------------------------------------------------
  /**
   * True while the backend has MASKED this meeting (it lives in a sealed,
   * not-session-unlocked folder). The template renders the lock gate instead
   * of the note/transcript/audio/timeline/actions. Mirrors `detail()?.locked`.
   */
  readonly locked = computed(() => this.detail()?.locked === true);
  /** True while an `unlockMeeting` biometric call is in flight (pending state). */
  readonly unlocking = signal(false);
  /** Focusable unlock button — focused after the gate renders (afterNextRender). */
  private readonly unlockButton =
    viewChild<ElementRef<HTMLButtonElement>>("unlockButton");

  // --- Move-to-folder popover ---------------------------------------------
  /** True while the folder-picker popover is open. */
  readonly moveOpen = signal(false);

  /**
   * Read-only folder badge for the header: the owning folder's name + exposure
   * (open / locked / session), or null when the note is at the vault root or the
   * folder isn't (yet) in the loaded tree. Reactive to both the meeting's
   * `folderId` and the folders store, so a move/lock updates it live.
   */
  readonly folderBadge = computed<{
    name: string;
    exposure: FolderExposure;
  } | null>(() => {
    const fid = this.detail()?.meeting.folderId ?? null;
    if (fid === null) {
      return null;
    }
    const node = this.findFolder(this.folders.tree(), fid);
    return node
      ? { name: node.name, exposure: this.folders.exposureOf(node) }
      : null;
  });

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

  // --- Export menu state ---------------------------------------------------
  /**
   * Transient success token for the export buttons. One of "", "md-copied",
   * "md-saved" or "audio-saved" — the matching button swaps its label briefly.
   */
  readonly exportMsg = signal("");
  /** True while a save dialog + export IPC call is in flight (disables saves). */
  readonly exporting = signal(false);
  /** Inline error surfaced when an export fails. */
  readonly exportError = signal("");
  /** Tracked so we can cancel the pending export-label reset on destroy. */
  private exportResetTimer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Whether this install keeps high-fidelity per-stream master archives (the
   * "Keep high-fidelity masters" setting). Loaded best-effort in ngOnInit; gates
   * the master-export actions, since a meeting only has masters when it was
   * recorded with this on. Install-global (not per-meeting), so the backend
   * stays the source of truth — it rejects a stream with no master (InvalidArg)
   * or a sealed folder (Locked), both surfaced as friendly inline messages.
   */
  readonly keepsMasters = signal(false);

  // --- Export Canvas (Obsidian .canvas board) ------------------------------
  /** True while an exportCanvas IPC call is in flight (disables the button). */
  readonly exportingCanvas = signal(false);
  /** The written .canvas path, shown briefly as a "Canvas saved" confirmation. */
  readonly canvasMsg = signal("");
  /** Inline error surfaced when the canvas export fails (e.g. no timeline yet). */
  readonly canvasError = signal("");
  /** Tracked so we can cancel the pending canvas-confirmation reset on destroy. */
  private canvasResetTimer: ReturnType<typeof setTimeout> | null = null;

  // --- Meeting tags (editable; persisted via set/getMeetingTags) -----------
  /** The meeting's current tags (loaded in ngOnInit; updated optimistically). */
  readonly tags = signal<string[]>([]);
  /** Working copy of the add-tag input (input (input) → signal). */
  readonly tagDraft = signal("");
  /** Disables chips + input while a setMeetingTags IPC call is in flight. */
  readonly tagsBusy = signal(false);
  /** Inline error surfaced when a tag add/remove fails. */
  readonly tagsError = signal("");

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

  /**
   * The persisted in-meeting assistant Q&A for this meeting, citations parsed
   * into vault/web shapes for rendering. Empty when the meeting is locked (the
   * backend gates `assistantInteractions` exactly like `note`/`segments`).
   */
  readonly interactions = computed<AssistantQa[]>(() => {
    const raw = this.detail()?.assistantInteractions ?? [];
    return raw.map((i, idx) => this.parseInteraction(i, idx));
  });

  // --- Phase 5 model-provenance badge -------------------------------------
  /**
   * Human-readable label for the model-provenance badge in the Analysis header.
   * Prefers `modelServed` (what the gateway actually ran) over `aiModel`
   * (what was requested). Returns null when no provenance is available (legacy
   * meetings, locked meetings, providers without `CallMeta`) — the badge is
   * hidden via `@if` in that case.
   */
  readonly provenanceLabel = computed<{ model: string; provider: string } | null>(() => {
    const d = this.detail();
    if (!d) return null;
    const model = d.modelServed ?? d.aiModel;
    const provider = d.aiProvider;
    if (!model && !provider) return null;
    return { model: model ?? "", provider: provider ?? "" };
  });

  // --- Interactive timeline (speaker + topic viz) -------------------------
  readonly timeline = signal<MeetingTimeline | null>(null);
  readonly timelineLoading = signal(false);
  readonly timelineError = signal(false);

  // --- Pin-this-moment (timeline (pin) → pinMoment IPC + clipboard) --------
  /** Transient confirmation after a successful pin, e.g. "Pinned 2:14 — …". */
  readonly pinMsg = signal("");
  /** Inline error surfaced when a pin (or its clipboard copy) fails. */
  readonly pinError = signal("");
  /** True while a pinMoment IPC call is in flight (debounces rapid clicks). */
  readonly pinning = signal(false);
  /** Tracked so we can cancel the pending pin-confirmation reset on destroy. */
  private pinResetTimer: ReturnType<typeof setTimeout> | null = null;

  // --- Connect-to-graph (linkMeetingEntities → People/ & Projects/ stubs) --
  /** True while a linkMeetingEntities IPC call is in flight. */
  readonly linking = signal(false);
  /** The resolved graph entities after a successful link (null until run). */
  readonly graph = signal<GraphPayload | null>(null);
  /** Inline error surfaced when the graph link fails. */
  readonly graphError = signal("");

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
    await this.loadMeeting(id);
  }

  /**
   * Navigate to a semantically-related meeting and reload the view in place.
   * The `/meeting/:id` route reuses THIS component (the default
   * RouteReuseStrategy keeps it when only the param changes), so a same-route
   * navigation does NOT re-run `ngOnInit` — we reload explicitly. The related
   * section then re-fetches via its `meetingId` input.
   */
  async openRelated(id: string): Promise<void> {
    if (!id || this.detail()?.meeting.id === id) {
      return;
    }
    await this.router.navigate(["/meeting", id]);
    await this.loadMeeting(id);
  }

  /**
   * Load (or reload) a meeting by id into the view. Resets the per-meeting
   * signals that aren't derived from `detail()` so an in-place reload never
   * shows the previous meeting's timeline/tags/graph or a stale open editor.
   * (Derived state — note/audio/interactions/folderBadge — recomputes off
   * `detail()` automatically.)
   */
  private async loadMeeting(id: string): Promise<void> {
    this.loading.set(true);
    // Clear non-derived per-meeting state for a clean same-route reload.
    this.timeline.set(null);
    this.timelineError.set(false);
    this.tags.set([]);
    this.graph.set(null);
    this.graphError.set("");
    this.editing.set(false);
    this.renaming.set(false);
    this.moveOpen.set(false);
    this.confirmingDelete.set(false);
    // Reset audio-playback signals so an in-place meeting→meeting nav never shows the
    // previous meeting's position/play-state until a media event self-corrects.
    this.playing.set(false);
    this.currentTime.set(0);
    this.duration.set(0);
    try {
      this.detail.set(await this.ipc.getMeetingDetail(id));
    } finally {
      this.loading.set(false);
    }
    // Whether this install keeps hi-res masters — gates the master-export
    // actions. Install-global, so load it regardless of lock state (best-effort;
    // a failure simply hides the actions). The backend remains the real gate.
    try {
      this.keepsMasters.set((await this.ipc.getConfig()).keepHiresMasters);
    } catch {
      this.keepsMasters.set(false);
    }
    // Locked (masked) meetings render the lock gate only — skip priming the
    // timeline/tags (they're empty/masked) and focus the Unlock button instead.
    if (this.locked()) {
      afterNextRender(() => this.unlockButton()?.nativeElement.focus(), {
        injector: this.injector,
      });
      return;
    }
    // Kick the timeline off after the detail load; never blocks the page and
    // tolerates the first-call LLM latency (backend caches the result).
    if (this.detail()) {
      void this.loadTimeline();
      // Prime the folder tree so the read-only folder/lock badge + the move
      // picker have state on a direct navigation (idempotent; the root component
      // also loads it). Non-blocking — a failure just hides the badge.
      void this.folders.load();
      // Load the meeting's tags (best-effort; failure leaves the chips empty).
      try {
        this.tags.set(await this.ipc.getMeetingTags(id));
      } catch {
        this.tags.set([]);
      }
    }
  }

  // --- Move to folder ------------------------------------------------------

  /** Open/close the folder-picker popover (closed while the detail is busy). */
  toggleMove(): void {
    if (this.busy()) {
      return;
    }
    this.moveOpen.update((v) => !v);
  }

  /** Dismiss the folder-picker popover. */
  closeMove(): void {
    this.moveOpen.set(false);
  }

  /**
   * Apply a completed move locally: patch the in-memory meeting's `folderId` so
   * the header badge updates immediately (the picker already moved it via the
   * service + reloaded the tree). Then close the popover.
   */
  onMoved(folderId: string | null): void {
    this.detail.update((d) =>
      d ? { ...d, meeting: { ...d.meeting, folderId } } : d,
    );
    this.closeMove();
  }

  /** Depth-first search for a folder node by id across the forest. */
  private findFolder(nodes: FolderNode[], id: string): FolderNode | null {
    for (const n of nodes) {
      if (n.id === id) {
        return n;
      }
      const hit = this.findFolder(n.children, id);
      if (hit) {
        return hit;
      }
    }
    return null;
  }

  // --- Phase 0.5 lock gate -------------------------------------------------

  /**
   * Unlock this meeting's owning folder via the biometric (Touch ID) path, then
   * RE-FETCH the now-unmasked detail and replace the `detail` signal in place so
   * the note/transcript/audio/timeline render. The IPC returning null (root /
   * already-open folder) is still treated as success — we re-fetch regardless.
   * On failure (biometric denied / cancelled / error) we surface a toast and
   * stay gated. Uses await (no subscribe-for-state); the button shows a pending
   * state while in flight. Once unmasked, the timeline + tags are primed too.
   */
  async unlock(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id || this.unlocking()) {
      return;
    }
    this.unlocking.set(true);
    try {
      // Run the biometric unlock_folder path for the meeting's folder.
      await this.ipc.unlockMeeting(id);
      // Re-fetch the now-unmasked detail and swap it in place. A null detail
      // (deleted out from under us) keeps the not-found state honest.
      const fresh = await this.ipc.getMeetingDetail(id);
      this.detail.set(fresh);
      if (fresh && !fresh.locked) {
        // Refresh the folder tree so the header lock badge reflects the unlock,
        // then prime the timeline + tags the masked load skipped. Non-blocking.
        void this.folders.load();
        void this.loadTimeline();
        try {
          this.tags.set(await this.ipc.getMeetingTags(id));
        } catch {
          this.tags.set([]);
        }
      }
    } catch {
      // Biometric denied / cancelled, or the unlock errored — stay gated.
      this.toast.danger(
        "Couldn’t unlock — authentication failed or cancelled.",
      );
    } finally {
      this.unlocking.set(false);
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

  /**
   * Pin the timeline's current moment: derive a short label (the topic span
   * under the playhead, else "Pinned moment"), call `pinMoment` to write a
   * `^block-ref` + obsidian:// deep link, copy the link to the clipboard, then
   * flash a brief confirmation. Errors surface inline; nothing else is touched.
   */
  async onPin(seconds: number): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id || this.pinning()) {
      return;
    }
    this.pinError.set("");
    this.pinning.set(true);
    try {
      const result = await this.ipc.pinMoment(
        id,
        seconds,
        this.pinLabel(seconds),
      );
      try {
        await navigator.clipboard.writeText(result.url);
      } catch {
        // Pin still landed in the note; only the clipboard copy was refused.
      }
      this.flashPin(`Pinned ${result.mmss} — Obsidian link copied`);
    } catch (e) {
      this.pinError.set("Couldn’t pin: " + String(e));
    } finally {
      this.pinning.set(false);
    }
  }

  /** Short pin label: the topic span containing `seconds`, else a default. */
  private pinLabel(seconds: number): string {
    const topic = this.timeline()?.topics.find(
      (t) => seconds >= t.startS && seconds < t.endS,
    );
    return topic?.label?.trim() || "Pinned moment";
  }

  /**
   * Apply a manual speaker re-label from the timeline legend (e.g. "User 1" →
   * "Sarah"): call `renameSpeaker`, then fold the returned timeline into the
   * `timeline` signal so the lanes + legend relabel immediately. Errors are
   * handled silently inline — the previous timeline stays put, no crash.
   */
  async onRenameSpeaker(change: {
    oldLabel: string;
    newLabel: string;
  }): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id) {
      return;
    }
    try {
      this.timeline.set(
        await this.ipc.renameSpeaker(id, change.oldLabel, change.newLabel),
      );
    } catch {
      // Keep the existing timeline; the relabel simply didn't take.
    }
  }

  /** Show the pin confirmation for a moment (tracked timeout — cancelled on destroy). */
  private flashPin(message: string): void {
    this.pinMsg.set(message);
    if (this.pinResetTimer) {
      clearTimeout(this.pinResetTimer);
    }
    this.pinResetTimer = setTimeout(() => this.pinMsg.set(""), 3200);
    this.destroyRef.onDestroy(() => {
      if (this.pinResetTimer) {
        clearTimeout(this.pinResetTimer);
      }
    });
  }

  /**
   * Connect this meeting to the Obsidian vault graph: resolve its people +
   * projects into `People/` / `Projects/` stub notes with backlinks, then show
   * the resolved entities as chips. Gated on a note existing. Errors inline.
   */
  async linkGraph(): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id || !this.note() || this.linking()) {
      return;
    }
    this.graphError.set("");
    this.linking.set(true);
    try {
      this.graph.set(await this.ipc.linkMeetingEntities(id));
    } catch (e) {
      this.graph.set(null);
      this.graphError.set("Couldn’t connect to graph: " + String(e));
    } finally {
      this.linking.set(false);
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

  // --- Meeting tags --------------------------------------------------------

  /** Mirror the add-tag input value into the `tagDraft` signal. */
  onTagInput(event: Event): void {
    this.tagDraft.set((event.target as HTMLInputElement).value);
  }

  /**
   * Add the typed tag: trim, ignore empty/duplicate (case-insensitive), then
   * persist the new array. Clears the input on a non-empty attempt.
   */
  async addTag(): Promise<void> {
    const tag = this.tagDraft().trim();
    if (!tag) {
      return;
    }
    const exists = this.tags().some(
      (t) => t.toLowerCase() === tag.toLowerCase(),
    );
    this.tagDraft.set("");
    if (exists) {
      return;
    }
    await this.persistTags([...this.tags(), tag]);
  }

  /** Remove a tag and persist the reduced array. */
  async removeTag(tag: string): Promise<void> {
    await this.persistTags(this.tags().filter((t) => t !== tag));
  }

  /**
   * Optimistically apply `next` to the `tags` signal, persist via
   * setMeetingTags, and roll back to the previous tags if the write fails.
   * Errors surface inline next to the editor.
   */
  private async persistTags(next: string[]): Promise<void> {
    const id = this.detail()?.meeting.id;
    if (!id) {
      return;
    }
    const previous = this.tags();
    this.tagsError.set("");
    this.tags.set(next);
    this.tagsBusy.set(true);
    try {
      await this.ipc.setMeetingTags(id, next);
    } catch (e) {
      this.tags.set(previous);
      this.tagsError.set("Couldn’t save tags: " + String(e));
    } finally {
      this.tagsBusy.set(false);
    }
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

  // --- Export menu ---------------------------------------------------------

  /**
   * Copy the note's raw markdown to the clipboard (the full source, not the
   * parsed analysis). Flashes a brief "Copied" confirmation on the button.
   */
  async copyMarkdown(): Promise<void> {
    if (this.editing()) {
      return;
    }
    const markdown = this.detail()?.note?.markdown;
    if (!markdown) {
      return;
    }
    this.exportError.set("");
    try {
      await navigator.clipboard.writeText(markdown);
      this.flashExport("md-copied");
    } catch (e) {
      this.exportError.set("Couldn’t copy: " + String(e));
    }
  }

  /**
   * Prompt for a destination via the native save dialog, then write the note
   * markdown there through `exportNote`. A dismissed dialog (null path) is a
   * no-op; failures surface inline.
   */
  async saveMarkdown(id: string, title: string | null): Promise<void> {
    if (this.editing() || this.exporting()) {
      return;
    }
    this.exportError.set("");
    this.exporting.set(true);
    try {
      const path = await save({
        defaultPath: `${this.sanitizeTitle(title)}.md`,
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (path) {
        await this.ipc.exportNote(id, path);
        this.flashExport("md-saved");
      }
    } catch (e) {
      this.exportError.set("Couldn’t save markdown: " + String(e));
    } finally {
      this.exporting.set(false);
    }
  }

  /**
   * Prompt for a destination via the native save dialog, then copy the meeting
   * recording (WAV) there through `exportAudio`. Only reachable when the
   * meeting actually has audio (the button is gated on `audioSrc()`).
   */
  async saveAudio(id: string, title: string | null): Promise<void> {
    if (this.editing() || this.exporting()) {
      return;
    }
    this.exportError.set("");
    this.exporting.set(true);
    try {
      const path = await save({
        defaultPath: `${this.sanitizeTitle(title)}.wav`,
        filters: [{ name: "Audio", extensions: ["wav"] }],
      });
      if (path) {
        await this.ipc.exportAudio(id, path);
        this.flashExport("audio-saved");
      }
    } catch (e) {
      this.exportError.set("Couldn’t save audio: " + String(e));
    } finally {
      this.exporting.set(false);
    }
  }

  /**
   * Prompt for a destination via the native save dialog, then copy the meeting's
   * hi-res master archive (faithful per-stream float32 WAV) there through the
   * gated `exportMicMaster` / `exportSysMaster` commands — the ONLY way these
   * archives leave the app. A dismissed dialog (null path) is a no-op. The
   * backend fails closed: a sealed-and-not-unlocked folder rejects with Locked,
   * and a stream that was never archived rejects with "no master" — both are
   * mapped to a clear, actionable message (never a crash).
   */
  async exportMaster(
    stream: "mic" | "sys",
    id: string,
    title: string | null,
  ): Promise<void> {
    if (this.editing() || this.exporting()) {
      return;
    }
    this.exportError.set("");
    this.exporting.set(true);
    try {
      const path = await save({
        defaultPath: `${this.sanitizeTitle(title)}.${stream}.wav`,
        filters: [{ name: "Audio", extensions: ["wav"] }],
      });
      if (path) {
        if (stream === "mic") {
          await this.ipc.exportMicMaster(id, path);
          this.flashExport("mic-master-saved");
        } else {
          await this.ipc.exportSysMaster(id, path);
          this.flashExport("sys-master-saved");
        }
      }
    } catch (e) {
      this.exportError.set(this.masterErrorMessage(stream, e));
    } finally {
      this.exporting.set(false);
    }
  }

  /**
   * Map a master-export failure to a clear message: a Locked folder → unlock to
   * export; a missing per-stream archive → none was kept; anything else verbatim.
   */
  private masterErrorMessage(stream: "mic" | "sys", error: unknown): string {
    const raw = String(error);
    if (/locked/i.test(raw)) {
      return "This meeting is locked — unlock it to export the master.";
    }
    if (/no master/i.test(raw)) {
      return stream === "mic"
        ? "No hi-res mic master was kept for this meeting."
        : "No hi-res system master was kept for this meeting.";
    }
    return "Couldn’t export the master: " + raw;
  }

  /**
   * Save-as-PDF via the OS print dialog. A body-level class flips on the print
   * stylesheet (isolating the note/analysis) for the duration of the synchronous
   * `window.print()` call, then is cleared so the live UI is untouched.
   */
  saveAsPdf(): void {
    if (this.editing()) {
      return;
    }
    document.body.classList.add("murmur-printing");
    try {
      window.print();
    } finally {
      document.body.classList.remove("murmur-printing");
    }
  }

  /**
   * Export this meeting as an Obsidian Canvas board: call `exportCanvas` (which
   * writes `vault/Canvas/<title>.canvas` and returns the path), then flash a
   * brief "Canvas saved" confirmation with that path. Gated on a parsed note
   * existing; errors (e.g. "open the meeting once to generate its timeline
   * first") surface inline and leave the rest of the page untouched.
   */
  async exportCanvas(id: string): Promise<void> {
    if (this.editing() || this.exportingCanvas() || !this.note()) {
      return;
    }
    this.canvasError.set("");
    this.exportingCanvas.set(true);
    try {
      const path = await this.ipc.exportCanvas(id);
      this.flashCanvas(path);
    } catch (e) {
      this.canvasError.set("Couldn’t export Canvas: " + String(e));
    } finally {
      this.exportingCanvas.set(false);
    }
  }

  /** Show the "Canvas saved" confirmation (tracked timeout — cancelled on destroy). */
  private flashCanvas(path: string): void {
    this.canvasMsg.set(path);
    if (this.canvasResetTimer) {
      clearTimeout(this.canvasResetTimer);
    }
    this.canvasResetTimer = setTimeout(() => this.canvasMsg.set(""), 4000);
    this.destroyRef.onDestroy(() => {
      if (this.canvasResetTimer) {
        clearTimeout(this.canvasResetTimer);
      }
    });
  }

  /**
   * Flash a transient success token on an export button (tracked timeout —
   * cancelled on destroy so we never poke a dead component).
   */
  private flashExport(token: string): void {
    this.exportMsg.set(token);
    if (this.exportResetTimer) {
      clearTimeout(this.exportResetTimer);
    }
    this.exportResetTimer = setTimeout(() => this.exportMsg.set(""), 2200);
    this.destroyRef.onDestroy(() => {
      if (this.exportResetTimer) {
        clearTimeout(this.exportResetTimer);
      }
    });
  }

  /** Build a filesystem-safe filename stem from a meeting title. */
  private sanitizeTitle(title: string | null): string {
    const cleaned = (title || "")
      .trim()
      .replace(/[\\/:*?"<>|]+/g, " ")
      .replace(/\s+/g, " ")
      .trim();
    return cleaned || "meeting-note";
  }

  // --- Markdown parsing ----------------------------------------------------

  /**
   * Strips a leading YAML front-matter block (between the first `---` and the
   * next `---`), pulls out `tags` + `participants`, then splits the remaining
   * body into `## ` sections. Falls back to raw markdown when no section is
   * found.
   */
  /**
   * Enrich a raw persisted interaction with a stable id + parsed citations. The
   * backend stores citations as plain strings: `[[Title]]` for a vault source,
   * or a bare URL / `(web)` marker for a web source. We split the two so the
   * template can render `[[vault]]` chips vs distinct "via web" links.
   */
  private parseInteraction(i: AssistantInteraction, idx: number): AssistantQa {
    return {
      id: `${i.createdAt}#${idx}`,
      command: i.command,
      answer: i.answer,
      citations: (i.citations ?? []).map((c) => this.parseCitation(c)),
      status: i.status,
      sourceLabel: i.sourceLabel,
      createdAt: i.createdAt,
    };
  }

  /** Split one persisted citation string into a vault- vs web-shaped chip. */
  private parseCitation(raw: string): ParsedCitation {
    const c = raw.trim();
    // A bare http(s) URL → web link.
    if (/^https?:\/\//i.test(c)) {
      return { kind: "web", label: this.hostOf(c) ?? c, url: c };
    }
    // `[[Title]]` (or `Title`) → vault chip; strip the wikilink brackets.
    const wiki = /^\[\[(.+?)\]\]$/.exec(c);
    if (wiki) {
      return { kind: "vault", label: wiki[1].trim(), url: null };
    }
    // `(web)` / `web` marker with no URL → a labelless web source.
    if (/^\(?web\)?$/i.test(c)) {
      return { kind: "web", label: "web", url: null };
    }
    // `Label (https://…)` form → web link with a friendly label.
    const labelled = /^(.*?)\s*\((https?:\/\/[^)]+)\)$/i.exec(c);
    if (labelled) {
      return {
        kind: "web",
        label: labelled[1].trim() || this.hostOf(labelled[2]) || labelled[2],
        url: labelled[2],
      };
    }
    // Fallback: treat as a vault title (no off-device origin implied).
    return { kind: "vault", label: c, url: null };
  }

  /** Best-effort host extraction for a web citation label; null if unparseable. */
  private hostOf(url: string): string | null {
    try {
      return new URL(url).host;
    } catch {
      return null;
    }
  }

  /** Map an interaction status to a global `.pill` variant (mirrors the live card). */
  protected qaStatusPillClass(status: string): string {
    switch (status) {
      case "ok":
        return "is-success";
      case "needs_consent":
        return "is-warning";
      case "unavailable":
      case "unrecognized":
        return "is-accent";
      case "nothing_heard":
        return "";
      default:
        return "is-danger";
    }
  }

  /** Short human label for the status pill. */
  protected qaStatusLabel(status: string): string {
    switch (status) {
      case "ok":
        return "Odpowiedziano";
      case "needs_consent":
        return "Wymaga zgody";
      case "unavailable":
        return "Niedostępne";
      case "unrecognized":
        return "Nierozpoznane";
      case "nothing_heard":
        return "Nic nie usłyszano";
      case "error":
        return "Błąd";
      default:
        return status;
    }
  }

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

  /**
   * Map a transcript segment's `speaker` to a small presentational chip:
   * "Me" (the local mic, accent) vs "Others" (captured system audio, neutral/
   * violet). Returns null for legacy / mic-only segments (`null` / unknown) so
   * they render unlabeled exactly as before. This is independent of the AI
   * timeline's manual speaker-rename — that feature relabels timeline lanes, not
   * these per-segment Me/Others tags.
   */
  speakerChip(
    speaker: Segment["speaker"],
  ): { label: string; bg: string; fg: string } | null {
    switch (speaker) {
      case "me":
        // Local mic — the calm accent.
        return {
          label: "Me",
          bg: "var(--accent-soft)",
          fg: "var(--accent-hover)",
        };
      case "others":
        // Captured system audio — a neutral violet, distinct from "Me".
        return {
          label: "Others",
          bg: "rgba(157, 123, 255, 0.16)",
          fg: "#b9a4ff",
        };
      default:
        return null;
    }
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
