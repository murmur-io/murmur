import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  effect,
  forwardRef,
  input,
  model,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { NG_VALUE_ACCESSOR, type ControlValueAccessor } from "@angular/forms";
import type { ModelCatalog } from "../../core/models";

/**
 * The ONE model picker.
 *
 * It existed three times before this: in `ai-setup-block`, in `ai-role-rows`, and again in the
 * gateway/Ollama connection cards. Every fix had to be made in each copy, and one of them was
 * missed for a full review round — the free-text id stayed hidden behind a non-empty catalog on the
 * role rows after being fixed on the Setup card.
 *
 * The behaviour it owns, in one place:
 *
 *   - options render their LABEL, never a raw id;
 *   - the free-text id is ALWAYS available, not only when the catalog is empty, because a bundled
 *     catalog is a hint and a model released after this build must still be selectable;
 *   - a value absent from the catalog is kept and shown as custom, never cleared;
 *   - Refresh appears unless the catalog is KNOWN bundled — `!== "bundled"` rather than
 *     `=== "live"`, so a failed fetch (no catalog at all) still offers the retry that is the whole
 *     point at that moment;
 *   - a bundled catalog says so instead of staying silent.
 *
 * ONE ControlValueAccessor per FormControl. The Setup card used to bind both a `<select>` and an
 * `<input>` to the same control; Angular writes with `{emitModelToViewChange: false}`, so a value
 * typed into one was never written back into the other's view. Both live inside this component now
 * and share one signal, which is what makes them agree.
 */
@Component({
  selector: "mur-model-picker",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./model-picker.component.html",
  styleUrl: "./model-picker.component.scss",
  providers: [
    {
      provide: NG_VALUE_ACCESSOR,
      useExisting: forwardRef(() => MurModelPickerComponent),
      multi: true,
    },
  ],
})
export class MurModelPickerComponent implements ControlValueAccessor {
  /** The connection's catalog. `undefined` = never fetched, or the fetch failed. */
  readonly catalog = input<ModelCatalog | undefined>(undefined);

  /** True while a `list_models` call for this connection is in flight. */
  readonly loading = input(false);

  /** Shown in the empty option: "Default (provider's pick)" vs "Default (connection's pick)". */
  readonly defaultLabel = input("Default (provider's pick)");

  /** Free-text placeholder — the two surfaces word "blank means default" differently. */
  readonly placeholder = input("Model id (blank = default)");

  readonly ariaLabel = input<string | null>(null);

  /** Emitted when the user asks for a re-fetch. The parent owns the IPC call. */
  readonly refresh = output<void>();

  /**
   * Emitted when the USER edits the value, as opposed to a programmatic `writeValue`.
   *
   * The role rows use it to dismiss a "this model was cleared because…" notice once the user has
   * chosen a replacement: without that, the explanation outlived the thing it explained. It is a
   * separate output rather than a `valueChanges` subscription in the parent because only the
   * component can tell a user edit from a form patch.
   */
  readonly modelEdited = output<void>();

  /** The selected model id. Two-way (`[(value)]`) AND the CVA's model. */
  readonly value = model("");

  readonly disabled = input(false);
  private readonly cvaDisabled = signal(false);
  readonly isDisabled = computed(() => this.disabled() || this.cvaDisabled());

  readonly options = computed(() => this.catalog()?.options ?? []);

  /**
   * Whether this connection HAS a live catalog to re-fetch — an ENGINE property, supplied by the
   * parent, never derived from `catalog.source`.
   *
   * Deriving it here was wrong and a spec already pins why: `claude_code` returning a bundled list
   * is not the same fact as `ollama` returning one. A source-derived rule makes Refresh appear or
   * vanish according to what the last fetch happened to return, so an Ollama daemon that answered
   * from cache would hide the button at the exact moment the user wants it. The connection decides;
   * the catalog only reports what came back.
   */
  readonly canRefresh = input(false);

  /**
   * A bundled catalog is a hint that may be stale, and the UI says so rather than implying currency.
   * Read from the CATALOG's provenance, not from any option's: an empty live catalog has no option
   * to carry a source, and must not be mislabelled as shipping with the app.
   */
  readonly showsBundledNotice = computed(
    () => !this.canRefresh() && this.options().length > 0,
  );

  /** A stored id the catalog does not list — kept selectable instead of silently dropped. */
  readonly valueIsCustom = computed(() => {
    const current = this.value();
    if (!current) return false;
    return !this.options().some((o) => o.id === current);
  });

  private readonly selectEl = viewChild<ElementRef<HTMLSelectElement>>("select");

  /**
   * Write the value onto the `<select>` DOM PROPERTY, after its options exist.
   *
   * `[value]="value()"` does not work here and the failure is silent: a `<select>` ignores a value
   * for which it has no matching `<option>`, and the property binding can run before the `@for`
   * has created them. The stored id then renders as an empty dropdown — the exact "the field shows
   * nothing while config holds a value" symptom this whole feature exists to remove.
   *
   * This effect re-runs on the value AND on the options, so the assignment always happens once the
   * matching option is present. Same lesson as the signal-CVA revert trap already recorded for this
   * codebase: a `ControlValueAccessor` built on signals must sync the DOM property itself.
   */
  private readonly _syncSelect = effect(() => {
    const el = this.selectEl()?.nativeElement;
    const current = this.value();
    // Track the options so a late catalog re-runs this.
    this.options();
    this.valueIsCustom();
    if (el && el.value !== current) el.value = current;
  });

  private onChange: (v: string) => void = () => undefined;
  private onTouched: () => void = () => undefined;

  writeValue(v: unknown): void {
    this.value.set(v == null ? "" : String(v));
  }
  registerOnChange(fn: (v: string) => void): void {
    this.onChange = fn;
  }
  registerOnTouched(fn: () => void): void {
    this.onTouched = fn;
  }
  setDisabledState(d: boolean): void {
    this.cvaDisabled.set(d);
  }

  /** Both controls write through here, so the select and the free-text input can never disagree. */
  private commit(next: string): void {
    this.value.set(next);
    this.onChange(next);
    this.onTouched();
    this.modelEdited.emit();
  }

  onSelect(e: Event): void {
    this.commit((e.target as HTMLSelectElement).value);
  }

  onType(e: Event): void {
    this.commit((e.target as HTMLInputElement).value);
  }

  onRefresh(): void {
    this.refresh.emit();
  }
}
