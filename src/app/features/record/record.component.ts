import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  OnInit,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { RecorderStore } from "../../core/recorder.store";
import { IpcService } from "../../core/ipc.service";
import type { Analytics, AppConfigDto } from "../../core/models";
import { MicMuteToggleComponent } from "./mic-mute-toggle.component";
import { MeetingConversationComponent } from "./meeting-conversation.component";
import { MeetingConversationStore } from "../../core/meeting-conversation.store";

@Component({
  selector: "app-record",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterLink,
    MicMuteToggleComponent,
    MeetingConversationComponent,
  ],
  host: { "(document:keydown)": "onKey($event)" },
  template: `
    <section class="record">
      @if (modelPresent() === false) {
        <div class="banner is-accent model-banner" role="alert">
          <span class="banner-icon" aria-hidden="true">↓</span>
          <div class="model-banner-body">
            <p class="model-banner-title">Transcription model needed</p>
            <p class="model-banner-text">
              Whisper runs on-device. Download it once to enable recording.
            </p>
            @if (modelDownloadError(); as derr) {
              <p class="model-banner-error">{{ derr }}</p>
            }
            <button
              type="button"
              class="btn btn-primary"
              (click)="downloadModel()"
              [disabled]="downloadingModel()"
            >
              {{ downloadingModel() ? "Downloading…" : "Download model" }}
            </button>
          </div>
        </div>
      } @else if (showVaultNotice()) {
        <!-- Calm, NON-blocking notice: notes are always saved in Murmur (the DB is the
             canonical store). A vault is optional and export-only — never a recording blocker. -->
        <div class="banner is-accent vault-notice" role="note">
          <span class="banner-icon" aria-hidden="true">i</span>
          <span class="vault-notice-body">
            No Obsidian vault set — your notes are saved in
            <strong>Murmur</strong>. Add a vault folder in Settings to also
            export them as Markdown.
          </span>
          <span class="vault-notice-actions">
            <a class="btn btn-ghost btn-sm" routerLink="/settings">Settings</a>
            <button
              type="button"
              class="btn btn-ghost btn-sm"
              (click)="dismissVaultNotice()"
            >
              Dismiss
            </button>
          </span>
        </div>
      }

      @if (headphonesHint()) {
        <div class="banner is-accent" role="note">
          <span class="banner-icon" aria-hidden="true">🎧</span>
          <span>
            Capturing system audio — use <strong>headphones</strong> so the
            other participants' voices don't echo back into your microphone.
          </span>
        </div>
      }

      <!-- ── Slim recording bar — ambient status, NOT the hero ─────────────── -->
      @if (store.isRecording()) {
        <!-- Recording is now ambient: a compact horizontal bar carrying the orb,
             timer, level meter, the LIVE caption ticker, mic-mute, Ask, and Stop.
             The conversation thread below is the hero. -->
        <div class="rec-strip is-recording" role="status">
          <span class="orb live" aria-hidden="true"></span>
          <span class="timer">{{ elapsedLabel() }}</span>
          <div class="wave" [style.--level]="store.level()" aria-hidden="true">
            @for (b of bars; track b) {
              <span class="wbar" [style.--i]="b"></span>
            }
          </div>

          <!-- Live caption ticker — rides inline in the strip. -->
          <p class="cc-line" aria-live="polite">
            @if (liveCaption(); as cc) {
              @for (rev of [cc]; track rev) {
                <span class="cc-text">{{ cc }}</span>
              }
            } @else {
              <span class="cc-idle">Listening…</span>
            }
          </p>

          <!-- Mic-mute: silences only the local mic; system audio keeps recording.
               Compact (icon-only) so the strip stays uncrowded. -->
          <app-mic-mute-toggle [compact]="true" #micToggle />
          <!-- Ask AI — CLICK-TO-STOP voice toggle. The spoken answer lands in the
               conversation thread below (the one home for asking mid-meeting). -->
          <button
            type="button"
            class="ask-btn"
            [class.is-listening]="assistant.listening()"
            [disabled]="assistant.processing()"
            (click)="toggleAsk()"
            [attr.aria-pressed]="assistant.listening()"
            [attr.aria-label]="
              assistant.listening()
                ? 'Stop listening and ask'
                : 'Ask the AI assistant'
            "
          >
            <svg
              class="ask-ico"
              viewBox="0 0 24 24"
              width="18"
              height="18"
              aria-hidden="true"
            >
              <path
                d="M12 3l1.6 4.4L18 9l-4.4 1.6L12 15l-1.6-4.4L6 9l4.4-1.6L12 3z"
                fill="currentColor"
              />
              <path
                d="M18.5 14l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8.8-2.2z"
                fill="currentColor"
                opacity="0.85"
              />
            </svg>
          </button>
          <button
            type="button"
            class="stop-btn"
            (click)="store.stop()"
            aria-label="Stop recording"
          >
            <span class="stop-ico" aria-hidden="true"></span>
          </button>
        </div>
      } @else {
        <!-- Not recording — a compact start header (nudge + brief + the start
             control). Still calm; the thread remains the main surface. -->
        <div class="rec-strip is-idle">
          <!-- Meeting-app nudge: a subtle, dismissible suggestion to hit record. -->
          @if (showNudge()) {
            <div class="nudge" role="status">
              <span class="nudge-dot" aria-hidden="true"></span>
              <span class="nudge-text">
                <strong>{{ detectedApp() }}</strong> is running — start recording?
              </span>
              <span class="nudge-actions">
                <button
                  type="button"
                  class="btn btn-primary btn-sm"
                  (click)="startFromNudge()"
                >
                  Start recording
                </button>
                <button
                  type="button"
                  class="btn btn-ghost btn-sm"
                  (click)="dismissNudge()"
                >
                  Dismiss
                </button>
              </span>
            </div>
          }

          @if (isProcessing()) {
            <div class="proc-inline" role="status">
              <span class="orb proc" aria-hidden="true"></span>
              <span class="proc-label">{{
                store.message() || store.stage()
              }}</span>
              <div class="proc-track" aria-hidden="true">
                <div class="proc-shimmer"></div>
              </div>
            </div>
          } @else {
            <button
              type="button"
              class="start-btn"
              (click)="store.start()"
              [disabled]="!canRecord()"
            >
              <span class="orb ready" aria-hidden="true"></span>
              <span class="start-text">
                {{
                  store.stage() === "done" ? "Record again" : "Start recording"
                }}
              </span>
              <span class="kbd" aria-hidden="true">⌘R</span>
            </button>
          }

          <span class="rec-strip-hint">{{ hint() }}</span>
          <button type="button" class="popout" (click)="popOut()">
            Pop out
            <span class="kbd-inline">⌘⇧R</span>
          </button>
        </div>
      }

      <!-- ── The notes + @brain threads surface — the full-height main view ── -->
      <!-- The main flow is the user's NOTES (persisted to manual_notes); the one
           composer splits a line by @brain (a plain note vs opening a thread). -->
      @if (showAssistant()) {
        <app-meeting-conversation
          class="conversation"
          [meetingId]="store.meetingId()"
          [hintsEnabled]="hintsEnabled()"
        />
      }

      @if (store.error(); as err) {
        @if (needsCloudConsent()) {
          <div class="banner is-accent cloud-consent" role="alert">
            <span class="banner-icon" aria-hidden="true">☁</span>
            <div class="cloud-consent-copy">
              <strong>Cloud processing isn't enabled</strong>
              <span>
                {{ providerLabel() }} sends your transcript (redacted first) to
                {{ cloudDestination() }} to write the summary — your data leaves
                this Mac. Allow it once to finish this note, or switch to a
                local provider in Settings to stay fully on-device.
              </span>
            </div>
            <div class="cloud-consent-actions">
              <button
                type="button"
                class="btn btn-primary"
                (click)="allowCloudAndRetry()"
                [disabled]="consenting()"
              >
                {{ consenting() ? "Enabling…" : "Allow & finish note" }}
              </button>
              <a class="btn btn-ghost" routerLink="/settings">Settings</a>
            </div>
          </div>
        } @else {
          <div class="banner is-danger" role="alert">
            <span class="banner-icon" aria-hidden="true">!</span>
            <span>{{ err }}</span>
          </div>
        }
      }

      <!-- ── Minimal stats strip — hidden once the conversation thread is the
           hero (recording / a live ask), so the thread fills the screen. ───── -->
      @if (!showAssistant() && analytics(); as a) {
        @if (a.totalMeetings > 0) {
          <div class="card stats" role="group" aria-label="Your stats">
            <dl class="figures">
              <div class="figure" style="--d: 0">
                <dt>Meetings</dt>
                <dd>{{ a.totalMeetings }}</dd>
              </div>
              <span class="sep" aria-hidden="true"></span>
              <div class="figure" style="--d: 1">
                <dt>Total time</dt>
                <dd>{{ formatDuration(a.totalDurationS) }}</dd>
              </div>
              <span class="sep" aria-hidden="true"></span>
              <div class="figure" style="--d: 2">
                <dt>This week</dt>
                <dd>{{ a.meetings7d }}</dd>
              </div>
            </dl>

            @if (spark().length > 0) {
              <div
                class="spark"
                aria-hidden="true"
                title="Meetings over the last 30 days"
              >
                @for (s of spark(); track s.date) {
                  <span class="spark-bar" [style.--h.%]="s.h"></span>
                }
              </div>
            }

            <a class="stats-link" routerLink="/analytics">
              View analytics
              <span class="arrow" aria-hidden="true">→</span>
            </a>
          </div>
        } @else {
          <div class="card stats stats-empty">
            <span class="stats-mark" aria-hidden="true"></span>
            <p class="stats-empty-text">Your stats will appear here</p>
            <a class="stats-link" routerLink="/analytics">
              View analytics
              <span class="arrow" aria-hidden="true">→</span>
            </a>
          </div>
        }
      }
    </section>
  `,
  styles: [
    `
      .record {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        /* Fill the routed viewport so the conversation thread can grow to the
           bottom (conversation-first). The host is the flex parent. */
        min-height: calc(100vh - var(--space-8));
        animation: rise 420ms var(--transition) both;
      }

      /* ── The conversation thread = the full-height main surface ─────────── */
      .conversation {
        display: block;
        flex: 1 1 auto;
        min-height: 0;
      }

      /* --- Model-download banner --- */
      .banner-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 24px;
        height: 24px;
        min-width: 24px;
        border-radius: 50%;
        background: rgba(255, 255, 255, 0.08);
        font-weight: 700;
        font-size: 0.85rem;
        line-height: 1;
      }
      .model-banner-body {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }

      /* --- Calm "no vault" info notice (non-blocking, dismissible) --- */
      .vault-notice {
        align-items: center;
        flex-wrap: wrap;
      }
      .vault-notice-body {
        flex: 1 1 14rem;
        min-width: 0;
      }
      .vault-notice-actions {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex: none;
        margin-left: auto;
      }

      /* --- Cloud-egress consent prompt (shown instead of a silent failure) --- */
      .cloud-consent {
        align-items: center;
        flex-wrap: wrap;
      }
      .cloud-consent-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        flex: 1 1 14rem;
        min-width: 0;
      }
      .cloud-consent-copy strong {
        color: var(--text-primary);
        font-weight: 600;
      }
      .cloud-consent-copy span {
        color: var(--text-secondary);
        font-size: 0.875rem;
        line-height: 1.5;
      }
      .cloud-consent-actions {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex: none;
      }
      .model-banner-title {
        margin: 0;
        font-weight: 600;
        color: var(--text-primary);
      }
      .model-banner-text {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.875rem;
      }
      .model-banner-error {
        margin: 0;
        color: var(--danger);
        font-size: 0.85rem;
      }
      .model-banner .btn {
        align-self: flex-start;
        margin-top: var(--space-1);
      }

      /* ── The slim recording strip — ambient status, full-width ─────────── */
      .rec-strip {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex: none;
        padding: var(--space-2) var(--space-3);
        border-radius: var(--radius-lg);
        border: 1px solid var(--glass-border);
        background: rgba(255, 255, 255, 0.04);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        box-shadow: var(--glass-highlight);
        animation: bar-in 320ms var(--ease-spring) both;
      }
      .rec-strip.is-recording {
        border-color: rgba(255, 122, 92, 0.4);
        box-shadow: var(--live-glow), var(--glass-highlight);
      }
      .rec-strip.is-idle {
        flex-wrap: wrap;
        gap: var(--space-3);
      }
      @keyframes bar-in {
        from {
          opacity: 0;
          transform: scale(0.98);
        }
        to {
          opacity: 1;
          transform: scale(1);
        }
      }

      /* Start control (idle) — compact pill, the whole thing is the button. */
      .start-btn {
        display: inline-flex;
        align-items: center;
        gap: var(--space-3);
        height: 44px;
        padding: 0 var(--space-3) 0 var(--space-4);
        border-radius: var(--radius-pill);
        border: 1px solid var(--glass-border);
        background: rgba(255, 255, 255, 0.05);
        color: var(--text-primary);
        font-family: inherit;
        cursor: pointer;
        transition:
          border-color var(--transition),
          background var(--transition),
          transform var(--transition-fast);
      }
      .start-btn:hover:not(:disabled) {
        transform: translateY(-1px);
        border-color: var(--border-strong);
        background: var(--surface-hover);
      }
      .start-btn:active:not(:disabled) {
        transform: translateY(0);
      }
      .start-btn:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .start-btn:disabled {
        opacity: 0.45;
        cursor: not-allowed;
      }
      .start-text {
        font-size: 0.95rem;
        font-weight: 550;
        letter-spacing: -0.01em;
      }
      .rec-strip-hint {
        flex: 1 1 12rem;
        min-width: 0;
        color: var(--text-muted);
        font-size: 0.85rem;
        letter-spacing: -0.005em;
      }
      .proc-inline {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex: none;
        min-width: 220px;
      }

      /* ── Meeting-app nudge: subtle accent strip, never blocking ────────── */
      .nudge {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex: 1 1 100%;
        min-width: 0;
        padding: var(--space-2) var(--space-2) var(--space-2) var(--space-4);
        border: 1px solid rgba(110, 118, 255, 0.3);
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        color: var(--text-primary);
        animation: rise 320ms var(--transition) both;
      }
      .nudge-dot {
        width: 8px;
        height: 8px;
        min-width: 8px;
        border-radius: 50%;
        background: var(--accent);
        box-shadow: 0 0 10px rgba(110, 118, 255, 0.8);
      }
      .nudge-text {
        flex: 1;
        min-width: 0;
        font-size: 0.875rem;
        line-height: 1.4;
      }
      .nudge-actions {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        flex: none;
      }
      .nudge .btn-sm {
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.85rem;
      }

      .kbd {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        height: 26px;
        padding: 0 var(--space-2);
        border-radius: var(--radius-sm);
        background: rgba(255, 255, 255, 0.07);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.78rem;
        font-weight: 500;
      }

      .timer {
        font-family: var(--font-mono);
        font-size: 0.95rem;
        font-weight: 500;
        font-variant-numeric: tabular-nums;
        letter-spacing: 0.02em;
        color: var(--text-primary);
        min-width: 48px;
        flex: none;
      }
      .wave {
        flex: 0 0 auto;
        display: flex;
        align-items: center;
        gap: 2px;
        width: 84px;
        height: 28px;
        /* Guard: bars can never spill past the fixed box into the caption line. */
        overflow: hidden;
      }
      .wbar {
        flex: 1;
        min-width: 2px;
        max-width: 5px;
        height: 100%;
        border-radius: var(--radius-pill);
        background: var(--live-gradient);
        transform: scaleY(0.16);
        transform-origin: center;
        animation: wave 1100ms ease-in-out infinite;
        animation-delay: calc(var(--i) * -78ms);
      }
      @keyframes wave {
        0%,
        100% {
          transform: scaleY(calc(0.14 + var(--level, 0) * 0.55));
        }
        50% {
          transform: scaleY(calc(0.32 + var(--level, 0) * 1.15));
        }
      }

      /* Stop — warm circular button with a rounded square glyph (slim). */
      .stop-btn {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 40px;
        height: 40px;
        min-width: 40px;
        flex: none;
        border: none;
        border-radius: 50%;
        background: var(--live-gradient);
        cursor: pointer;
        box-shadow: 0 8px 24px rgba(255, 94, 120, 0.45);
        transition:
          transform var(--transition-fast),
          filter var(--transition);
      }
      .stop-btn:hover {
        filter: brightness(1.08);
        transform: scale(1.04);
      }
      .stop-btn:active {
        transform: scale(0.97);
      }
      .stop-btn:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px rgba(255, 122, 92, 0.6);
      }
      .stop-ico {
        width: 17px;
        height: 17px;
        border-radius: 5px;
        background: #fff;
      }

      /* Ask AI — sparkle button beside Stop. Calm accent at idle, pulsing glow
         while the assistant is listening so it's unmistakable. */
      .ask-btn {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 40px;
        height: 40px;
        min-width: 40px;
        flex: none;
        border: 1px solid var(--accent-ring);
        border-radius: 50%;
        color: var(--accent-hover);
        background: var(--accent-soft);
        cursor: pointer;
        transition:
          transform var(--transition-fast),
          background var(--transition),
          box-shadow var(--transition),
          color var(--transition);
      }
      .ask-btn:hover {
        background: var(--accent);
        color: #fff;
        transform: scale(1.05);
      }
      .ask-btn:active {
        transform: scale(0.96);
      }
      .ask-btn:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .ask-btn.is-listening {
        background: var(--accent-gradient);
        color: #fff;
        border-color: transparent;
        animation: ask-pulse 1.5s ease-in-out infinite;
      }
      .ask-ico {
        display: block;
      }
      @keyframes ask-pulse {
        0%,
        100% {
          box-shadow: 0 0 0 0 var(--accent-ring);
        }
        50% {
          box-shadow: 0 0 0 8px rgba(110, 118, 255, 0);
        }
      }

      .ask-btn:disabled {
        opacity: 0.55;
        cursor: default;
      }

      @media (prefers-reduced-motion: reduce) {
        .ask-btn.is-listening {
          animation: none;
        }
      }

      /* Processing — cool, calm, working (inline in the idle strip). */
      .proc-label {
        flex: none;
        color: var(--text-primary);
        font-size: 0.92rem;
        font-weight: 550;
        text-transform: capitalize;
      }
      .proc-track {
        flex: 1;
        min-width: 80px;
        height: 4px;
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.08);
        overflow: hidden;
      }
      .proc-shimmer {
        height: 100%;
        width: 40%;
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
        animation: shimmer 1.3s ease-in-out infinite;
      }
      @keyframes shimmer {
        0% {
          transform: translateX(-120%);
        }
        100% {
          transform: translateX(320%);
        }
      }

      /* ── Status orb (left node) ────────────────────────────────────────── */
      .orb {
        position: relative;
        width: 14px;
        height: 14px;
        min-width: 14px;
        border-radius: 50%;
      }
      .orb.ready {
        background: var(--accent);
        box-shadow: 0 0 12px rgba(110, 118, 255, 0.8);
      }
      .orb.ready::after {
        content: "";
        position: absolute;
        inset: -5px;
        border-radius: 50%;
        border: 1.5px solid var(--accent);
        opacity: 0.5;
        animation: breathe 2.4s ease-in-out infinite;
      }
      @keyframes breathe {
        0%,
        100% {
          transform: scale(1);
          opacity: 0.5;
        }
        50% {
          transform: scale(1.5);
          opacity: 0;
        }
      }
      .orb.live {
        background: var(--live);
        box-shadow: 0 0 14px rgba(255, 122, 92, 0.9);
        animation: live-pulse 1.4s ease-in-out infinite;
      }
      @keyframes live-pulse {
        0%,
        100% {
          opacity: 1;
          transform: scale(1);
        }
        50% {
          opacity: 0.55;
          transform: scale(0.82);
        }
      }
      .orb.proc {
        background: transparent;
        border: 2px solid rgba(255, 255, 255, 0.18);
        border-top-color: var(--accent);
        animation: spin 0.8s linear infinite;
      }
      @keyframes spin {
        to {
          transform: rotate(360deg);
        }
      }

      .popout {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        flex: none;
        margin-left: auto;
        padding: var(--space-1) var(--space-3);
        border: 1px solid var(--border);
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.03);
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.82rem;
        font-weight: 500;
        cursor: pointer;
        transition:
          background var(--transition),
          border-color var(--transition),
          color var(--transition),
          transform var(--transition-fast);
      }
      .popout:hover {
        background: var(--surface-hover);
        border-color: var(--border-strong);
        color: var(--text-primary);
      }
      .popout:active {
        transform: translateY(1px);
      }
      .popout:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }

      /* ── Live caption ticker — rides inline in the recording strip ─────── */
      .cc-line {
        flex: 1 1 auto;
        min-width: 0;
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.875rem;
        line-height: 1.4;
        overflow: hidden;
        white-space: nowrap;
        text-overflow: ellipsis;
      }
      .cc-text {
        display: block;
        overflow: hidden;
        text-overflow: ellipsis;
        animation: cc-in 260ms var(--transition) both;
      }
      .cc-idle {
        color: var(--text-muted);
        font-style: italic;
      }
      @keyframes cc-in {
        from {
          opacity: 0;
          transform: translateY(4px);
        }
        to {
          opacity: 1;
          transform: translateY(0);
        }
      }

      /* ── Minimal stats strip ───────────────────────────────────────────── */
      .stats {
        display: flex;
        align-items: center;
        gap: var(--space-5);
        padding: var(--space-4) var(--space-5);
      }

      .figures {
        display: flex;
        align-items: center;
        gap: var(--space-4);
        margin: 0;
        flex: none;
      }
      .figure {
        display: flex;
        flex-direction: column;
        gap: 2px;
        animation: rise 420ms var(--transition) both;
        animation-delay: calc(120ms + var(--d) * 70ms);
      }
      .figure dt {
        color: var(--text-muted);
        font-size: 0.6875rem;
        font-weight: 550;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }
      .figure dd {
        margin: 0;
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 1.15rem;
        font-weight: 500;
        font-variant-numeric: tabular-nums;
        letter-spacing: -0.01em;
        line-height: 1.1;
      }
      .sep {
        width: 1px;
        height: 26px;
        background: var(--border-subtle);
        flex: none;
      }

      /* Tiny inline CSS sparkline of perDay activity. */
      .spark {
        display: flex;
        align-items: flex-end;
        gap: 2px;
        flex: 1;
        min-width: 0;
        height: 30px;
        padding: 0 var(--space-1);
        overflow: hidden;
      }
      .spark-bar {
        flex: 1;
        min-width: 2px;
        height: max(2px, var(--h, 0%));
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
        opacity: 0.85;
        transform-origin: bottom;
        animation: spark-grow 520ms var(--ease-spring) both;
      }
      @keyframes spark-grow {
        from {
          transform: scaleY(0);
          opacity: 0;
        }
        to {
          transform: scaleY(1);
          opacity: 0.85;
        }
      }

      .stats-link {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        flex: none;
        margin-left: auto;
        color: var(--text-secondary);
        font-size: 0.85rem;
        font-weight: 550;
        white-space: nowrap;
        transition: color var(--transition);
      }
      .stats-link .arrow {
        transition: transform var(--transition);
      }
      .stats-link:hover {
        color: var(--text-primary);
      }
      .stats-link:hover .arrow {
        transform: translateX(3px);
      }
      .stats-link:focus-visible {
        outline: none;
        border-radius: var(--radius-sm);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }

      /* Empty state — soft, quiet, still offers the way in. */
      .stats-empty {
        justify-content: flex-start;
        gap: var(--space-3);
      }
      .stats-mark {
        width: 30px;
        height: 30px;
        min-width: 30px;
        border-radius: 50%;
        background: var(--surface-input);
        border: 1px solid var(--border);
        flex: none;
      }
      .stats-empty-text {
        margin: 0;
        color: var(--text-muted);
        font-size: 0.9rem;
      }

      .kbd-inline {
        font-family: var(--font-mono);
        font-size: 0.8em;
        padding: 1px 6px;
        border-radius: 6px;
        background: rgba(255, 255, 255, 0.07);
        border: 1px solid var(--border);
        color: var(--text-secondary);
      }
    `,
  ],
})
export class RecordComponent implements OnInit {
  readonly store = inject(RecorderStore);
  /** The in-meeting NOTES + @brain THREADS store. Injected + init()'d here (not only from
   * the surface) so it subscribes to the wake/result streams even before the surface shows. */
  readonly assistant = inject(MeetingConversationStore);
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  // NOTE: "clear the conversation on a new recording" now lives in
  // MeetingConversationStore.setMeetingId (keyed on the meeting id, which survives
  // navigation), NOT in a per-component isRecording-edge effect. The old effect
  // wiped the thread when you left the record tab and came back mid-recording,
  // because its edge state (a plain field) reset to false on component re-mount.

  /** Name of a running meeting app (Zoom/Teams/Webex), or null if none detected. */
  readonly detectedApp = signal<string | null>(null);
  /** Once dismissed, the nudge stays hidden for the rest of this session. */
  private readonly nudgeDismissed = signal(false);

  /** Handle for the meeting-app poll — cleared on destroy (no leaked interval). */
  private meetingAppPoll: ReturnType<typeof setInterval> | null = null;

  /** Bars in the live waveform (driven by the real mic level signal). 16 bars fit the fixed
   * 84px `.wave` box (16×2px min + 15×2px gap ≈ 62px) with room to flex; 28 overflowed the box
   * and spilled into the caption ("…IIIIDzięki za oglądanie!"). */
  readonly bars = Array.from({ length: 16 }, (_, i) => i);

  /** The in-pill mic-mute toggle — its `muted()` signal drives the stage hint. */
  private readonly micToggle = viewChild(MicMuteToggleComponent);

  /** Latest partial transcript, trimmed — drives the ephemeral caption line. */
  readonly liveCaption = computed(() => this.store.liveCaption().trim());

  /** Latest settings snapshot, refreshed on entry — used for the readiness guard. */
  private readonly config = signal<AppConfigDto | null>(null);

  /** Best-effort: is the default output the built-in speakers? null/undetermined ⇒ assume yes. */
  private readonly onSpeakers = signal<boolean>(true);

  /** Headphones hint: system-audio capture + built-in speakers = echo into the mic (rec #5). */
  readonly headphonesHint = computed(
    () => (this.config()?.captureSystemAudio ?? false) && this.onSpeakers(),
  );

  /**
   * Proactive brain hints (the global mute, default ON). Gates the recall card
   * in the conversation surface; the backend mutes the event source too when
   * off — this is the render-side half of the belt and braces.
   */
  readonly hintsEnabled = computed(
    () => this.config()?.proactiveHintsEnabled ?? true,
  );

  /**
   * Show the conversation thread — the full-height main surface of the
   * conversation-first record screen. It is the home for BOTH note-taking AND
   * the agent, so it surfaces during ANY recording (notes always persist via
   * `save_manual_notes`, even when the brain backend is off — only the @brain
   * agent path is then unavailable), whenever realtime reactions are enabled,
   * and whenever a manual "Ask AI" is listening / in flight (so the answer has a
   * home). The thread itself subscribes to the wake/result streams regardless.
   */
  readonly showAssistant = computed(() => {
    const c = this.config();
    // Mirror the LIVE role resolver: an explicit roleLiveConnection wins over
    // the legacy brainBackend fallback (Ask=Off compat-writes brainBackend and
    // must not hide a Live surface that is explicitly a cloud provider).
    const liveConn = c ? c.roleLiveConnection || c.brainBackend : "";
    const enabled = !!c && c.realtimeReactions === true && liveConn !== "off";
    return (
      enabled ||
      this.store.isRecording() ||
      this.assistant.listening() ||
      this.assistant.processing() ||
      this.assistant.manualAskInFlight()
    );
  });

  /**
   * True when no Obsidian vault folder is configured. The vault is EXPORT-ONLY — every note
   * is always saved to Murmur's canonical DB — so this NO LONGER blocks recording. It only
   * drives the calm, dismissible "set a vault to also export" info notice + the "done" hint copy.
   */
  readonly vaultMissing = computed(() => {
    const c = this.config();
    return !c || !c.vaultPath || c.vaultPath.trim() === "";
  });

  /** Dismissed for this session — the no-vault info notice stays hidden once closed. */
  private readonly vaultNoticeDismissed = signal(false);

  /**
   * Show the calm, non-blocking "no vault set" info notice: only when no vault is configured
   * and the user hasn't dismissed it this session. It never gates recording.
   */
  readonly showVaultNotice = computed(
    () => this.vaultMissing() && !this.vaultNoticeDismissed(),
  );

  /**
   * True when the last failure was the backend's cloud-egress consent gate. We
   * detect the stable "cloud egress not consented" marker from `make_provider`
   * and surface a friendly consent prompt instead of the raw error banner —
   * never a silent failure.
   */
  readonly needsCloudConsent = computed(() => {
    const e = this.store.error();
    return !!e && /cloud egress not consented/i.test(e);
  });

  /** Human label for the configured provider (for the consent copy). */
  readonly providerLabel = computed(() => {
    switch (this.config()?.providerId) {
      case "anthropic":
        return "The Anthropic API";
      case "claude_code":
        return "Claude Code";
      case "gateway":
        return "AI Gateway";
      case "ollama":
        return "Ollama";
      default:
        return "This provider";
    }
  });

  /**
   * Human name of the destination the redacted transcript goes to (for the
   * consent copy). This banner only shows after the backend's fail-closed
   * `egress_is_cloud` gate refused (`needsCloudConsent`), so the provider is
   * cloud-classified by definition — for ollama that means the base URL is
   * non-loopback, hence "your remote Ollama server" without re-parsing it here.
   */
  readonly cloudDestination = computed(() => {
    switch (this.config()?.providerId) {
      case "anthropic":
      case "claude_code":
        return "Anthropic's cloud";
      case "gateway":
        return "your AI gateway";
      case "ollama":
        return "your remote Ollama server";
      default:
        return "your provider's cloud";
    }
  });

  /** True while the one-time consent command + retry are in flight. */
  readonly consenting = signal(false);

  /** Real Whisper-model presence (null = checking). */
  readonly modelPresent = signal<boolean | null>(null);
  readonly downloadingModel = signal(false);
  readonly modelDownloadError = signal<string | null>(null);

  /** Busy but not capturing audio → transcribing / summarizing / exporting. */
  readonly isProcessing = computed(
    () => this.store.isBusy() && !this.store.isRecording(),
  );

  /**
   * Whether the Record action is allowed right now. A missing vault does NOT block recording
   * (the note is always saved to Murmur; the vault is export-only) — only a missing/downloading
   * model or an in-flight pipeline gates it.
   */
  readonly canRecord = computed(
    () =>
      this.modelPresent() !== false &&
      !this.downloadingModel() &&
      !this.store.isBusy(),
  );

  /**
   * Show the start-recording nudge only when a meeting app is running, we're
   * not already recording, the user hasn't dismissed it this session, and
   * recording is actually possible. A nudge — never blocks the screen.
   */
  readonly showNudge = computed(
    () =>
      this.detectedApp() !== null &&
      !this.store.isRecording() &&
      !this.nudgeDismissed() &&
      this.canRecord(),
  );

  /** Elapsed recording time as m:ss. */
  readonly elapsedLabel = computed(() => {
    const s = this.store.elapsed();
    const m = Math.floor(s / 60);
    const sec = (s % 60).toString().padStart(2, "0");
    return `${m}:${sec}`;
  });

  /** Context line beneath the bar. */
  readonly hint = computed(() => {
    if (this.store.isRecording()) {
      // When the mic is muted the recording continues from system audio only —
      // make that unmistakable in the prominent hint line beneath the pill.
      if (this.micToggle()?.muted())
        return "Mic muted — still capturing others. Press ⌘R or Stop when done.";
      return "Recording — press ⌘R or Stop when done.";
    }
    if (this.isProcessing()) return "Transcribing on-device, then summarizing…";
    if (this.modelPresent() === false) return "Download the model to start.";
    if (this.store.stage() === "done")
      return this.vaultMissing()
        ? "Saved ✓ — your note is in Murmur."
        : "Saved ✓ — your note is in the vault.";
    return "On-device transcription · your audio never leaves this Mac.";
  });

  /** Aggregate stats for the minimal home strip (null = not yet loaded). */
  readonly analytics = signal<Analytics | null>(null);

  /**
   * Sparkline bars: per-day meeting counts scaled to a 0–100% height, padded to
   * a steady width so a single busy day doesn't render as one lonely bar.
   */
  readonly spark = computed<{ date: string; h: number }[]>(() => {
    const a = this.analytics();
    if (!a || a.perDay.length === 0) return [];
    const days = [...a.perDay].sort((x, y) => x.date.localeCompare(y.date));
    const max = Math.max(...days.map((d) => d.count), 1);
    const bars = days.map((d) => ({
      date: d.date,
      h: Math.round((d.count / max) * 100),
    }));
    // Left-pad with flat placeholders so the strip keeps a consistent rhythm.
    const minBars = 14;
    if (bars.length < minBars) {
      const pad = Array.from({ length: minBars - bars.length }, (_, i) => ({
        date: `pad-${i}`,
        h: 0,
      }));
      return [...pad, ...bars];
    }
    return bars;
  });

  async ngOnInit(): Promise<void> {
    await this.store.init();
    // Subscribe the notes/threads store to the wake/result + BOTH tool-trace
    // streams now, regardless of whether the surface is visible yet — otherwise
    // events fired before it renders (or while the config snapshot is stale) drop.
    void this.assistant.init();
    this.config.set(await this.ipc.getConfig());
    void this.ipc.outputIsBuiltinSpeakers().then((v) => this.onSpeakers.set(v ?? true));
    this.modelPresent.set(await this.ipc.modelPresent());
    // Stats are secondary — never let a failure here block the record screen.
    try {
      this.analytics.set(await this.ipc.getAnalytics());
    } catch {
      this.analytics.set(null);
    }

    // Meeting-app detection: check once now, then poll on a tracked interval.
    void this.checkMeetingApp();
    this.meetingAppPoll = setInterval(
      () => void this.checkMeetingApp(),
      12_000,
    );
    this.destroyRef.onDestroy(() => {
      if (this.meetingAppPoll !== null) {
        clearInterval(this.meetingAppPoll);
        this.meetingAppPoll = null;
      }
    });
  }

  /** Best-effort poll for a running meeting app; failures leave the nudge hidden. */
  private async checkMeetingApp(): Promise<void> {
    try {
      this.detectedApp.set(await this.ipc.detectMeetingApp());
    } catch {
      this.detectedApp.set(null);
    }
  }

  /** Nudge primary action — kick off a recording, then let it fade out. */
  startFromNudge(): void {
    void this.store.start();
  }

  /** Nudge ghost action — hide it for the rest of this session. */
  dismissNudge(): void {
    this.nudgeDismissed.set(true);
  }

  /** No-vault info-notice dismiss — hide it for the rest of this session. */
  dismissVaultNotice(): void {
    this.vaultNoticeDismissed.set(true);
  }

  /** ⌘R / Ctrl+R toggles recording. */
  onKey(e: KeyboardEvent): void {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "r") {
      e.preventDefault();
      if (this.store.isRecording()) {
        void this.store.stop();
      } else if (this.canRecord()) {
        void this.store.start();
      }
    }
  }

  /** Summon the floating always-on-top bar (also bound to ⌘⇧R globally). */
  popOut(): void {
    void this.ipc.toggleBar();
  }

  /**
   * CLICK-TO-STOP toggle for the "Ask AI" (✨) button. First click opens the
   * voice-command listener (no wake phrase); a second click — while listening —
   * stops it so the FULL utterance is dispatched (→ processing). The backend
   * streams listening/processing over EVENT_VOICE_COMMAND_LISTENING /
   * EVENT_VOICE_COMMAND_PROCESSING and the spoken answer lands in a thread on the
   * notes surface below. Swallow rejections (e.g. brain backend off) — the store
   * resets its listening/processing/in-flight state on error.
   */
  toggleAsk(): void {
    if (this.assistant.listening()) {
      void this.assistant.endAsk().catch(() => {
        /* stop failed — store cleared processing/in-flight */
      });
    } else {
      void this.assistant.askNow().catch(() => {
        /* listener unavailable — store resets the in-flight/listening state */
      });
    }
  }

  /**
   * Grant the one-time cloud-egress consent, then retry summarizing the meeting
   * that just failed the gate. The transcript is already captured + on disk, so a
   * `resummarize` finishes the note without re-recording. After consent we refresh
   * the config snapshot so `providerLabel` / readiness reflect the new state.
   */
  async allowCloudAndRetry(): Promise<void> {
    this.consenting.set(true);
    try {
      await this.ipc.consentToCloudEgress();
      this.config.set(await this.ipc.getConfig());
      const id = this.store.meetingId();
      if (id) {
        await this.store.resummarize(id);
      }
    } catch {
      // The store surfaces a fresh error banner on a failed retry; nothing to do here.
    } finally {
      this.consenting.set(false);
    }
  }

  /** Download the Whisper model, then re-check presence. */
  async downloadModel(): Promise<void> {
    this.modelDownloadError.set(null);
    this.downloadingModel.set(true);
    try {
      await this.ipc.downloadModel();
      this.modelPresent.set(await this.ipc.modelPresent());
    } catch (e) {
      this.modelDownloadError.set(String(e));
    } finally {
      this.downloadingModel.set(false);
    }
  }

  /** Presentational only: seconds → compact "1h 5m" / "12m" / "45s". */
  formatDuration(durationS: number): string {
    const total = Math.max(0, Math.round(durationS));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) {
      return `${h}h ${m}m`;
    }
    if (m > 0) {
      return `${m}m`;
    }
    return `${s}s`;
  }
}
