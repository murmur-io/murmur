import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { type Meta, type StoryObj, moduleMetadata } from "@storybook/angular";

import { MurSelectComponent } from "./select.component";

const meta: Meta<MurSelectComponent> = {
  title: "Components/Forms/Select",
  component: MurSelectComponent,
  tags: ["autodocs"],
  decorators: [moduleMetadata({ imports: [ReactiveFormsModule] })],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-select>` — a native select as a form control with **projected** `<option>`s " +
          "(static or `@for`-generated). Options paint `var(--surface-overlay)` (rule T3).\n\n" +
          "Two ways to drive it, and they write the same `value` model:\n" +
          "- the CVA path — `formControlName` / `[formControl]`;\n" +
          "- the signal path — `[(value)]` + `[disabled]`, for hosts with no reactive form.\n\n" +
          "`[disabled]` and a disabled FormControl are OR-ed: either one disables.",
      },
    },
  },
  argTypes: {
    value: { control: "inline-radio", options: ["small", "medium", "large-v3"] },
    disabled: { control: "boolean" },
    ariaLabel: { control: "text" },
    valueChange: { action: "valueChange" },
  },
  args: { value: "medium", disabled: false, ariaLabel: "Transcription model" },
  render: (args) => ({
    props: args,
    template: `
      <div style="max-width: 320px">
        <mur-select [(value)]="value" [disabled]="disabled" [ariaLabel]="ariaLabel" (valueChange)="valueChange($event)">
          <option value="small">Whisper small — fastest</option>
          <option value="medium">Whisper medium — balanced</option>
          <option value="large-v3">Whisper large-v3 — most accurate</option>
        </mur-select>
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<MurSelectComponent>;

/** The signal path: `[(value)]` + `[disabled]`. */
export const SignalBound: Story = {};
export const Disabled: Story = { args: { disabled: true } };

/** The CVA path: a `FormControl` drives value and disabled state. */
export const ReactiveForm: Story = {
  parameters: { controls: { disable: true } },
  render: () => ({
    props: { control: new FormControl("en") },
    template: `
      <div style="max-width: 320px; display: grid; gap: var(--space-2)">
        <mur-select [formControl]="control" ariaLabel="Language">
          <option value="auto">Detect automatically</option>
          <option value="en">English</option>
          <option value="pl">Polski</option>
          <option value="de">Deutsch</option>
        </mur-select>
        <span style="color: var(--text-tertiary); font-size: 12px">Form value: {{ control.value }}</span>
      </div>`,
  }),
};
