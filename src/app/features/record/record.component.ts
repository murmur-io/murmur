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
import { PreMeetingBriefComponent } from "./pre-meeting-brief.component";
import { MicMuteToggleComponent } from "./mic-mute-toggle.component";
import { AssistantActionsComponent } from "./assistant-actions.component";

@Component({
  selector: "app-record",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterLink,
    PreMeetingBriefComponent,
    MicMuteToggleComponent,
    AssistantActionsComponent,
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

      @if (headphonesHint() && !store.isRecording()) {
        <div class="banner is-accent" role="note">
          <span class="banner-icon" aria-hidden="true">🎧</span>
          <span>
            Capturing system audio — use <strong>headphones</strong> so the
            other participants' voices don't echo back into your microphone.
          </span>
        </div>
      }

      <!-- ── The morphing recording bar (the hero) ───────────────────────── -->
      <div class="stage" [class.live]="store.isRecording()">
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

        <!-- Pre-meeting brief: a subtle prep affordance — only when not recording. -->
        @if (showBrief()) {
          <app-pre-meeting-brief
            [initialSubject]="briefPrefill()"
            (dismissed)="dismissBrief()"
          />
        }

        @if (store.isRecording()) {
          <div class="rec-bar is-recording" role="status">
            <span class="orb live" aria-hidden="true"></span>
            <span class="timer">{{ elapsedLabel() }}</span>
            <div
              class="wave"
              [style.--level]="store.level()"
              aria-hidden="true"
            >
              @for (b of bars; track b) {
                <span class="wbar" [style.--i]="b"></span>
              }
            </div>
            <!-- Mic-mute: silences only the local mic; system audio keeps
                 recording. Sits beside Stop but never starts/stops anything.
                 Compact (icon-only) so the pill stays uncrowded; the descriptive
                 "still capturing others" copy rides the stage hint below. -->
            <app-mic-mute-toggle [compact]="true" #micToggle />
            <button
              type="button"
              class="stop-btn"
              (click)="store.stop()"
              aria-label="Stop recording"
            >
              <span class="stop-ico" aria-hidden="true"></span>
            </button>
          </div>
        } @else if (isProcessing()) {
          <div class="rec-bar is-processing" role="status">
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
            class="rec-bar is-ready"
            (click)="store.start()"
            [disabled]="!canRecord()"
          >
            <span class="orb ready" aria-hidden="true"></span>
            <span class="ready-text">
              {{
                store.stage() === "done" ? "Record again" : "Ready to record"
              }}
            </span>
            <span class="kbd" aria-hidden="true">⌘R</span>
          </button>
        }

        <p class="stage-hint">{{ hint() }}</p>
        <button type="button" class="popout" (click)="popOut()">
          Pop out floating bar
          <span class="kbd-inline">⌘⇧R</span>
        </button>
      </div>

      <!-- ── Live captions — ephemeral partial transcript while recording ──── -->
      @if (store.isRecording()) {
        <div class="captions" role="group" aria-label="Live captions">
          <span class="cc-pill" aria-hidden="true">
            <span class="cc-dot"></span>
            LIVE
          </span>
          <p class="cc-line" aria-live="polite">
            @if (liveCaption(); as cc) {
              <!-- Keyed so each new partial replays the gentle fade/slide. -->
              @for (rev of [cc]; track rev) {
                <span class="cc-text">{{ cc }}</span>
              }
            } @else {
              <span class="cc-idle">Listening…</span>
            }
          </p>
        </div>
      }

      <!-- ── In-meeting voice assistant — recent actions (Phase H) ────────── -->
      @if (showAssistant()) {
        <app-assistant-actions />
      }

      @if (store.error(); as err) {
        @if (needsCloudConsent()) {
          <div class="banner is-accent cloud-consent" role="alert">
            <span class="banner-icon" aria-hidden="true">☁</span>
            <div class="cloud-consent-copy">
              <strong>Cloud processing isn't enabled</strong>
              <span>
                {{ providerLabel() }} sends your (redacted) transcript to
                Anthropic's cloud to write the summary — your data leaves this
                Mac. Allow it once to finish this note, or switch to Ollama in
                Settings to stay fully on-device.
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

      <!-- ── Minimal stats strip (home hero — links to full analytics) ────── -->
      @if (analytics(); as a) {
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
        gap: var(--space-5);
        animation: rise 420ms var(--transition) both;
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

      /* ── Stage: the hero area with an atmospheric glow ─────────────────── */
      .stage {
        position: relative;
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-4);
        padding: var(--space-8) var(--space-4) var(--space-7);
      }
      .stage::before {
        content: "";
        position: absolute;
        top: 8%;
        left: 50%;
        width: 460px;
        height: 260px;
        transform: translateX(-50%);
        border-radius: 50%;
        background: radial-gradient(
          closest-side,
          rgba(110, 118, 255, 0.28),
          transparent 72%
        );
        filter: blur(28px);
        opacity: 0.7;
        transition:
          background var(--transition),
          opacity var(--transition);
        pointer-events: none;
      }
      /* When recording, the glow warms + intensifies. */
      .stage.live::before {
        background: radial-gradient(
          closest-side,
          rgba(255, 110, 100, 0.32),
          transparent 72%
        );
        opacity: 0.95;
      }

      /* ── Meeting-app nudge: subtle accent strip, never blocking ────────── */
      .nudge {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        width: 100%;
        max-width: 560px;
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

      /* ── The capsule, shared across states (each state swaps content) ──── */
      .rec-bar {
        position: relative;
        display: flex;
        align-items: center;
        gap: var(--space-3);
        height: 72px;
        padding: 0 var(--space-3) 0 var(--space-5);
        border-radius: var(--radius-pill);
        border: 1px solid var(--glass-border);
        background: rgba(255, 255, 255, 0.05);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        box-shadow: var(--shadow-lg), var(--glass-highlight);
        animation: bar-in 360ms var(--ease-spring) both;
        transition:
          width var(--transition),
          border-color var(--transition),
          box-shadow var(--transition),
          transform var(--transition-fast);
      }
      @keyframes bar-in {
        from {
          opacity: 0;
          transform: scale(0.94);
        }
        to {
          opacity: 1;
          transform: scale(1);
        }
      }

      /* Ready — the whole capsule is the button. */
      .rec-bar.is-ready {
        width: 340px;
        max-width: 100%;
        cursor: pointer;
        color: var(--text-primary);
        font-family: inherit;
      }
      .rec-bar.is-ready:hover:not(:disabled) {
        transform: translateY(-2px);
        border-color: var(--border-strong);
        box-shadow:
          0 32px 80px rgba(0, 0, 0, 0.6),
          0 0 0 1px var(--accent-soft),
          var(--glass-highlight);
      }
      .rec-bar.is-ready:active:not(:disabled) {
        transform: translateY(0);
      }
      .rec-bar.is-ready:focus-visible {
        outline: none;
        box-shadow:
          0 0 0 3px var(--accent-ring),
          var(--shadow-lg);
      }
      .rec-bar.is-ready:disabled {
        opacity: 0.45;
        cursor: not-allowed;
      }
      .ready-text {
        flex: 1;
        text-align: left;
        font-size: 1.0625rem;
        font-weight: 550;
        letter-spacing: -0.01em;
      }
      .kbd {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        height: 28px;
        padding: 0 var(--space-3);
        border-radius: var(--radius-sm);
        background: rgba(255, 255, 255, 0.07);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.8rem;
        font-weight: 500;
      }

      /* Recording — warm, alive. */
      .rec-bar.is-recording {
        width: 520px;
        max-width: 100%;
        border-color: rgba(255, 122, 92, 0.4);
        box-shadow: var(--live-glow), var(--shadow-lg), var(--glass-highlight);
      }
      .timer {
        font-family: var(--font-mono);
        font-size: 1.0625rem;
        font-weight: 500;
        font-variant-numeric: tabular-nums;
        letter-spacing: 0.02em;
        color: var(--text-primary);
        min-width: 56px;
      }
      .wave {
        flex: 1;
        display: flex;
        align-items: center;
        gap: 3px;
        height: 36px;
        padding: 0 var(--space-2);
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

      /* Stop — warm circular button with a rounded square glyph. */
      .stop-btn {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 52px;
        height: 52px;
        min-width: 52px;
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

      /* Processing — cool, calm, working. */
      .rec-bar.is-processing {
        width: 420px;
        max-width: 100%;
      }
      .proc-label {
        flex: none;
        color: var(--text-primary);
        font-size: 0.95rem;
        font-weight: 550;
        text-transform: capitalize;
      }
      .proc-track {
        flex: 1;
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

      .stage-hint {
        margin: 0;
        min-height: 1.2em;
        color: var(--text-muted);
        font-size: 0.875rem;
        letter-spacing: -0.005em;
      }
      .popout {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        margin-top: var(--space-1);
        padding: var(--space-2) var(--space-4);
        border: 1px solid var(--border);
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.03);
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.85rem;
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

      /* ── Live captions — frosted, secondary, ephemeral ─────────────────── */
      .captions {
        display: flex;
        align-items: flex-start;
        gap: var(--space-3);
        width: 100%;
        max-width: 560px;
        margin: 0 auto;
        padding: var(--space-3) var(--space-4);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-lg);
        background: rgba(255, 255, 255, 0.035);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        box-shadow: var(--glass-highlight);
        animation: rise 360ms var(--transition) both;
      }
      .cc-pill {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        flex: none;
        margin-top: 1px;
        padding: 2px var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--live-soft);
        color: var(--live-hover);
        font-size: 0.625rem;
        font-weight: 700;
        letter-spacing: 0.08em;
        line-height: 1.4;
      }
      .cc-dot {
        width: 6px;
        height: 6px;
        border-radius: 50%;
        background: var(--live);
        box-shadow: 0 0 8px rgba(255, 122, 92, 0.9);
        animation: live-pulse 1.4s ease-in-out infinite;
      }
      .cc-line {
        flex: 1;
        min-width: 0;
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.9rem;
        line-height: 1.5;
      }
      .cc-text {
        display: block;
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
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  /** Name of a running meeting app (Zoom/Teams/Webex), or null if none detected. */
  readonly detectedApp = signal<string | null>(null);
  /** Once dismissed, the nudge stays hidden for the rest of this session. */
  private readonly nudgeDismissed = signal(false);

  /** Title of the next calendar event (best-effort prefill), or null. */
  private readonly nextEventTitle = signal<string | null>(null);
  /** Once dismissed, the prep card stays hidden for the rest of this session. */
  private readonly briefDismissed = signal(false);
  /** Handle for the meeting-app poll — cleared on destroy (no leaked interval). */
  private meetingAppPoll: ReturnType<typeof setInterval> | null = null;

  /** Bars in the live waveform (driven by the real mic level signal). */
  readonly bars = Array.from({ length: 28 }, (_, i) => i);

  /** The in-pill mic-mute toggle — its `muted()` signal drives the stage hint. */
  private readonly micToggle = viewChild(MicMuteToggleComponent);

  /** Latest partial transcript, trimmed — drives the ephemeral caption line. */
  readonly liveCaption = computed(() => this.store.liveCaption().trim());

  /** Latest settings snapshot, refreshed on entry — used for the readiness guard. */
  private readonly config = signal<AppConfigDto | null>(null);

  /** Headphones hint: capturing system audio through speakers echoes into the mic (rec #5). */
  readonly headphonesHint = computed(
    () => this.config()?.captureSystemAudio ?? false,
  );

  /**
   * Show the in-meeting voice-assistant card (Phase H): only when the user has
   * enabled "realtime reactions" AND the brain backend isn't off. The card
   * itself subscribes to the wake/result streams regardless of recording state
   * (a wake can land any time the listener is active).
   */
  readonly showAssistant = computed(() => {
    const c = this.config();
    return !!c && c.realtimeReactions === true && c.brainBackend !== "off";
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
      default:
        return "This provider";
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

  /**
   * Subject to prefill the prep card: the next calendar event's title first,
   * else the detected meeting-app name (if the nudge already surfaces one),
   * else empty so the user types their own.
   */
  readonly briefPrefill = computed(
    () => this.nextEventTitle() ?? this.detectedApp() ?? "",
  );

  /**
   * Show the prep affordance only when NOT recording / processing and the user
   * hasn't dismissed it this session. Kept subtle — it never competes with the
   * record hero, and is hidden the moment a recording (or its processing) runs.
   */
  readonly showBrief = computed(
    () =>
      !this.store.isRecording() &&
      !this.isProcessing() &&
      !this.briefDismissed(),
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
    this.config.set(await this.ipc.getConfig());
    this.modelPresent.set(await this.ipc.modelPresent());
    // Stats are secondary — never let a failure here block the record screen.
    try {
      this.analytics.set(await this.ipc.getAnalytics());
    } catch {
      this.analytics.set(null);
    }

    // Next-event prefill for the prep card — purely best-effort; on any failure
    // (or no upcoming event) we simply leave the subject blank for the user.
    try {
      const next = await this.ipc.nextCalendarEvent();
      this.nextEventTitle.set(next?.title ?? null);
    } catch {
      this.nextEventTitle.set(null);
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

  /** Prep-card dismiss — hide it for the rest of this session. */
  dismissBrief(): void {
    this.briefDismissed.set(true);
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
