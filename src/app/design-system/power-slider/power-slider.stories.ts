import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { type Meta, type StoryObj, moduleMetadata } from "@storybook/angular";

import { MurPowerSliderComponent, type PowerRung } from "./power-slider.component";

const RUNGS: readonly PowerRung[] = [
  { id: "battery", name: "Battery saver" },
  { id: "balanced", name: "Balanced" },
  { id: "sharp", name: "Sharp" },
  { id: "max", name: "Maximum" },
];

const meta: Meta<MurPowerSliderComponent> = {
  title: "Components/Forms/Power slider",
  component: MurPowerSliderComponent,
  tags: ["autodocs"],
  decorators: [moduleMetadata({ imports: [ReactiveFormsModule] })],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-power-slider>` — a DISCRETE ladder as a range control, sharing `mur-slider`’s look " +
          "through the `.mur-range` primitive.\n\n" +
          "- **Preview vs commit.** Dragging previews the rung; `value` (and the form) updates once, " +
          "on release — dragging past Maximum never persists every rung on the way.\n" +
          "- **Accessibility.** `aria-valuetext` reads the rung *name*; PageUp/PageDown jump exactly " +
          "one rung.\n" +
          "- Driven by a `FormControl` **or** `[(value)]` + `[disabled]`, like `<mur-select>`. A value " +
          "not on the ladder is kept (shown as off-ladder), never silently replaced.",
      },
    },
  },
  argTypes: {
    value: { control: "inline-radio", options: RUNGS.map((r) => r.id) },
    disabled: { control: "boolean" },
    ariaLabel: { control: "text" },
    valueChange: { action: "valueChange" },
  },
  args: { rungs: RUNGS, value: "balanced", disabled: false, ariaLabel: "Transcription power" },
  render: (args) => ({
    props: args,
    template: `
      <div style="max-width: 420px">
        <mur-power-slider [rungs]="rungs" [(value)]="value" [disabled]="disabled"
          [ariaLabel]="ariaLabel" (valueChange)="valueChange($event)" />
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<MurPowerSliderComponent>;

export const SignalBound: Story = {};
export const Disabled: Story = { args: { disabled: true } };
export const OffLadderValue: Story = { args: { value: "custom-q5" } };

export const ReactiveForm: Story = {
  parameters: { controls: { disable: true } },
  render: () => ({
    props: { rungs: RUNGS, control: new FormControl("sharp") },
    template: `
      <div style="max-width: 420px; display: grid; gap: var(--space-2)">
        <mur-power-slider [rungs]="rungs" [formControl]="control" ariaLabel="Transcription power" />
        <span style="color: var(--text-tertiary); font-size: 12px">Committed: {{ control.value }}</span>
      </div>`,
  }),
};
