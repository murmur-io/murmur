import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../settings.store";

/** Settings → Storage: recordings location + usage, the GB cap, opt-in auto-prune, and a
 *  manual "Free up space". Notes/transcripts are never deleted — only audio. */
@Component({
  selector: "app-settings-storage-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="section-stack" [formGroup]="form">
      @if (report(); as r) {
        <!-- Usage summary -->
        <div class="card">
          <div class="usage-head">
            <span class="usage-total">{{ mb(r.usedBytes) }}</span>
            @if (r.limitBytes !== null) {
              <span class="text-muted"> / {{ mb(r.limitBytes) }}</span>
            }
          </div>
          @if (r.limitBytes !== null) {
            <div class="usage-bar" [attr.data-state]="barState()">
              <span class="usage-fill" [style.width.%]="pct()"></span>
            </div>
          }
          <div class="usage-legend text-secondary">
            {{ r.recordingCount }} recordings ·
            playback {{ mb(r.playbackBytes) }} ·
            masters {{ mb(r.mastersBytes) }} ·
            locked {{ mb(r.sealedBytes) }}
          </div>
          <div class="usage-path">
            <code class="path">{{ r.audioDir }}</code>
            <button type="button" class="btn btn-ghost" (click)="reveal()">Reveal in Finder</button>
          </div>
        </div>
      } @else {
        <div class="card"><p class="text-muted">Loading storage usage…</p></div>
      }

      <!-- Cap (GB) -->
      <div class="card">
        <label class="field">
          <span class="field-label">Storage limit (GB)</span>
          <input
            type="number"
            min="1"
            step="1"
            inputmode="numeric"
            placeholder="No limit"
            formControlName="audioStorageLimitGb"
          />
          <span class="field-help text-muted">
            The most disk your recordings may use. Leave blank for no limit. Notes and
            transcripts are always kept — only audio counts here.
          </span>
        </label>
      </div>

      <!-- Auto-prune toggle -->
      <div class="card">
        <label class="toggle-row">
          <span class="toggle-copy">
            <span class="toggle-title">Automatically delete old recordings</span>
            <span class="text-secondary toggle-sub">
              When over the limit, delete the OLDEST recordings' audio to make room
              (heavy masters first). Your notes and transcripts are never deleted, and
              recordings in locked folders are never touched.
            </span>
          </span>
          <input type="checkbox" formControlName="audioAutoPrune" />
        </label>
      </div>

      <!-- Manual free-up -->
      <div class="card">
        <div class="freeup-row">
          <div class="toggle-copy">
            <span class="toggle-title">Free up space now</span>
            <span class="text-secondary toggle-sub">
              @if (!hasCap()) {
                Set a limit above to enable this.
              } @else {
                Delete oldest recordings' audio down to the limit right now. This can't be undone.
              }
            </span>
            @if (lastFreed(); as f) {
              <span class="pill is-success">Freed {{ mb(f) }}</span>
            }
          </div>
          <button
            type="button"
            class="btn"
            [disabled]="storageBusy() || !hasCap()"
            (click)="onFreeUp()"
          >
            {{ storageBusy() ? "Freeing…" : "Free up space" }}
          </button>
        </div>
      </div>
    </div>
  `,
  styles: [
    `
      :host { display: contents; }
      .section-stack { display: flex; flex-direction: column; gap: var(--space-5); }
      .field { display: flex; flex-direction: column; gap: var(--space-1); }
      .field-label { color: var(--text-secondary); font-size: 0.9rem; font-weight: 550; }
      .field-help { font-size: 0.8125rem; line-height: 1.5; }
      input[type="number"] {
        width: 8rem; height: 34px; padding: 0 var(--space-3);
        border: 1px solid var(--border); border-radius: var(--radius-md);
        background: var(--surface-input); color: var(--text-primary); font: inherit;
      }
      .usage-head { display: flex; align-items: baseline; gap: var(--space-1); }
      .usage-total { font-size: 1.35rem; font-weight: 650; letter-spacing: -0.01em; }
      .usage-bar {
        margin: var(--space-2) 0; height: 8px; border-radius: var(--radius-pill);
        background: var(--surface-input); overflow: hidden;
      }
      .usage-fill { display: block; height: 100%; background: var(--accent); border-radius: inherit; }
      .usage-bar[data-state="amber"] .usage-fill { background: var(--warning, #d9a441); }
      .usage-bar[data-state="red"] .usage-fill { background: var(--live, #e5484d); }
      .usage-legend { font-size: 0.8125rem; margin-top: var(--space-1); }
      .usage-path { display: flex; align-items: center; gap: var(--space-3); margin-top: var(--space-3); flex-wrap: wrap; }
      .path {
        flex: 1 1 12rem; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
        font-size: 0.8rem; color: var(--text-secondary);
        background: var(--surface-input); padding: var(--space-1) var(--space-2); border-radius: var(--radius-sm);
      }
      .toggle-row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-4); cursor: pointer; }
      .toggle-copy { display: flex; flex-direction: column; gap: var(--space-1); }
      .toggle-title { color: var(--text-primary); font-size: 0.95rem; font-weight: 550; }
      .toggle-sub { font-size: 0.85rem; }
      .freeup-row { display: flex; align-items: center; justify-content: space-between; gap: var(--space-4); }
    `,
  ],
})
export class SettingsStorageSectionComponent implements OnInit {
  private readonly store = inject(SettingsStore);
  readonly form = this.store.form;
  readonly report = this.store.storageReport;
  readonly storageBusy = this.store.storageBusy;
  readonly lastFreed = this.store.lastFreed;

  /** % fill of the cap bar (0..100, clamped). */
  readonly pct = computed(() => {
    const r = this.report();
    if (!r || r.limitBytes == null || r.limitBytes === 0) return 0;
    return Math.min(100, Math.round((r.usedBytes / r.limitBytes) * 100));
  });
  /** Bar color state by fill. */
  readonly barState = computed(() => {
    const p = this.pct();
    return p >= 95 ? "red" : p >= 75 ? "amber" : "ok";
  });
  /** True when a storage cap is configured (drives the "Free up space" enablement). */
  readonly hasCap = computed(() => {
    const r = this.report();
    return !!r && r.limitBytes !== null;
  });

  ngOnInit(): void {
    // Ensure the report is fresh when the section mounts (load() already fetched it once).
    void this.store.loadStorageReport();
  }

  /** Human MB/GB label (binary). */
  mb(bytes: number): string {
    if (bytes >= 1024 * 1024 * 1024) return (bytes / (1024 * 1024 * 1024)).toFixed(2) + " GB";
    return Math.round(bytes / (1024 * 1024)) + " MB";
  }

  reveal(): void {
    this.store.revealAudioDir();
  }

  onFreeUp(): void {
    if (!confirm("Delete oldest recordings' audio to free up space? Notes are kept. This can't be undone.")) return;
    void this.store.freeUpSpace();
  }
}
