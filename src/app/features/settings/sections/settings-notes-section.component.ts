import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../settings.store";

/**
 * Settings → notes section (Stage-1 split): the `@case ("notes")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-notes-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="section-stack" [formGroup]="form">
              <div class="card notes-card">
                <div class="notes-copy">
                  <h3>Notes</h3>
                  <p class="text-secondary notes-sub">
                    Shape how your AI provider writes each summary and where it lands
                    in your vault.
                  </p>
                </div>

                <label class="field">
                  <span class="field-label">Summary style</span>
                  <select formControlName="noteStyle">
                    <option value="standard">Standard (balanced)</option>
                    <option value="brief">Brief (TL;DR + actions)</option>
                    <option value="detailed">Detailed (full depth)</option>
                    <option value="action">Action-focused</option>
                  </select>
                  <span class="field-help text-muted">
                    @switch (form.controls.noteStyle.value) {
                      @case ("brief") {
                        A tight TL;DR up top, then just the decisions and action items.
                      }
                      @case ("detailed") {
                        The full picture — discussion, context, decisions and every
                        follow-up.
                      }
                      @case ("action") {
                        Front-loads who-does-what — owners, tasks and due dates first.
                      }
                      @default {
                        A balanced summary, key points and action items — good for most
                        meetings.
                      }
                    }
                  </span>
                </label>

                <label class="field">
                  <span class="field-label">Your typed notes</span>
                  <select formControlName="notesMode">
                    <option value="enhance">
                      Enhance — your notes become the outline (recommended)
                    </option>
                    <option value="append">Append — keep them verbatim below</option>
                  </select>
                  <span class="field-help text-muted">
                    @switch (form.controls.notesMode.value) {
                      @case ("append") {
                        The summary is written from the transcript alone; your typed
                        notes are added verbatim as a "My notes" section at the end.
                      }
                      @default {
                        Your in-meeting bullets become the skeleton of the note — kept
                        in your words and order, expanded with detail from the
                        transcript, plus an "Also discussed" section for anything you
                        didn't jot down. Notes pass the same redaction firewall as the
                        transcript before any cloud call.
                      }
                    }
                    Meetings where you typed nothing are identical in both modes.
                  </span>
                </label>

                <label class="field">
                  <span class="field-label">Notes language</span>
                  <select formControlName="noteLanguage">
                    <option value="auto">Auto — match the meeting</option>
                    <option value="en">English</option>
                    <option value="pl">Polski</option>
                    <option value="de">Deutsch</option>
                    <option value="es">Español</option>
                    <option value="fr">Français</option>
                    <option value="it">Italiano</option>
                    <option value="pt">Português</option>
                    <option value="uk">Українська</option>
                    <option value="nl">Nederlands</option>
                  </select>
                  <span class="field-help text-muted">
                    @if (form.controls.noteLanguage.value === "auto") {
                      The whole note (headings + content) is written in the meeting's
                      language.
                    } @else {
                      The whole note is written in this language, whatever was spoken.
                    }
                  </span>
                </label>

                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Organize into thematic subfolders</span>
                    <span class="text-secondary toggle-sub">
                      Your AI provider files each note into a topic subfolder of your
                      vault (e.g. Standups, 1-1s, Acme Project).
                    </span>
                  </span>
                  <input type="checkbox" formControlName="autoOrganize" />
                </label>
              </div>
    </div>
  `,
  styles: [
    `
      /* Stage-1 split: the host stays layout-transparent so this section's
         cards remain direct flex items of the shell's .section-body (identical
         spacing to the pre-split monolith); .section-stack reproduces the
         .section-body column gap between this section's own cards. */
      :host {
        display: contents;
      }
      .section-stack {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* --- Notes card (summary style + auto-organize) --- */
      .notes-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .notes-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .notes-copy h3 {
        margin: 0;
      }
      .notes-sub {
        margin: 0;
        font-size: 0.875rem;
      }
      /* One-line helper that tracks the selected summary style. */
      .field-help {
        font-size: 0.8125rem;
        line-height: 1.5;
      }

      /* --- Stacked label + control --- */
      .field {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .field-label {
        color: var(--text-secondary);
        font-size: 0.9rem;
        font-weight: 550;
      }

      /* --- Capture-system-audio toggle row --- */
      .toggle-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        cursor: pointer;
      }
      .toggle-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .toggle-title {
        color: var(--text-primary);
        font-size: 0.95rem;
        font-weight: 550;
      }
      .toggle-sub {
        font-size: 0.85rem;
      }
    `,
  ],
})
export class SettingsNotesSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
}
