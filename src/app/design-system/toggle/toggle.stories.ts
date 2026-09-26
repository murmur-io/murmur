import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { type Meta, type StoryObj, moduleMetadata } from "@storybook/angular";

import { MurToggleComponent } from "./toggle.component";

const meta: Meta<MurToggleComponent> = {
  title: "Components/Forms/Toggle",
  component: MurToggleComponent,
  tags: ["autodocs"],
  decorators: [moduleMetadata({ imports: [ReactiveFormsModule] })],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-toggle>` — the macOS-style ON/OFF switch as a form control (ControlValueAccessor), " +
          "drawn by the global `.switch` primitive. It has no `[(value)]` — bind a `FormControl`. " +
          "`writeValue` also syncs the NATIVE checkbox, so a confirm-then-revert (user flips, backend " +
          "refuses, form sets it back) never leaves the switch showing the rejected state.",
      },
    },
  },
  argTypes: { ariaLabel: { control: "text" } },
  args: { ariaLabel: "Record system audio" },
  render: (args) => ({
    props: { ...args, control: new FormControl(true) },
    template: `
      <label style="display: flex; align-items: center; justify-content: space-between; gap: var(--space-4);
                    max-width: 360px; color: var(--text-primary)">
        Record system audio
        <mur-toggle [formControl]="control" [ariaLabel]="ariaLabel" />
      </label>
      <p style="color: var(--text-tertiary); font-size: 12px">Form value: {{ control.value }}</p>`,
  }),
};
export default meta;
type Story = StoryObj<MurToggleComponent>;

export const On: Story = {};

export const Off: Story = {
  render: (args) => ({
    props: { ...args, control: new FormControl(false) },
    template: `<mur-toggle [formControl]="control" [ariaLabel]="ariaLabel" />`,
  }),
};

export const Disabled: Story = {
  render: (args) => ({
    props: { ...args, control: new FormControl({ value: true, disabled: true }) },
    template: `<mur-toggle [formControl]="control" [ariaLabel]="ariaLabel" />`,
  }),
};
