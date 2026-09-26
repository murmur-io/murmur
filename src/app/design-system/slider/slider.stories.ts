import type { Meta, StoryObj } from "@storybook/angular";

import { MurSliderComponent } from "./slider.component";

const meta: Meta<MurSliderComponent> = {
  title: "Components/Forms/Slider",
  component: MurSliderComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-slider>` — the Liquid Glass range slider (pill track, round specular thumb, the " +
          "shared `.mur-range` primitive). Two-way bind `[(value)]`. For a DISCRETE named ladder use " +
          "`<mur-power-slider>` instead.",
      },
    },
  },
  argTypes: {
    value: { control: { type: "range", min: 0, max: 100 } },
    min: { control: "number" },
    max: { control: "number" },
    step: { control: "number" },
    disabled: { control: "boolean" },
    ariaLabel: { control: "text" },
    valueChange: { action: "valueChange" },
  },
  args: { value: 60, min: 0, max: 100, step: 1, disabled: false, ariaLabel: "Window transparency" },
  render: (args) => ({
    props: args,
    template: `
      <div style="max-width: 320px; display: grid; gap: var(--space-2)">
        <mur-slider [(value)]="value" [min]="min" [max]="max" [step]="step" [disabled]="disabled"
          [ariaLabel]="ariaLabel" (valueChange)="valueChange($event)" />
        <span style="color: var(--text-tertiary); font-size: 12px">{{ value }}</span>
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<MurSliderComponent>;

export const Default: Story = {};
export const Stepped: Story = { args: { min: 0, max: 10, step: 1, value: 4 } };
export const Disabled: Story = { args: { disabled: true } };
