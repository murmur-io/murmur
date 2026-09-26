import type { Meta, StoryObj } from "@storybook/angular";

import { MurMeterComponent } from "./meter.component";

const meta: Meta<MurMeterComponent> = {
  title: "Components/Data display/Meter",
  component: MurMeterComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-meter>` — a segmented indicator for a COARSE, ordinal quantity (accuracy, speed) " +
          "where a percentage would be a lie. Deliberately not `<mur-progress>`: the host carries " +
          "`role=\"img\"` and announces one sentence (“Accuracy: 4 of 4”), never a number that reads " +
          "as a percentage. `value` is clamped into `0..max`, `max` into `0..10`.",
      },
    },
  },
  argTypes: {
    value: { control: { type: "range", min: 0, max: 10, step: 1 } },
    max: { control: { type: "range", min: 1, max: 10, step: 1 } },
    label: { control: "text" },
    detail: { control: "text" },
  },
  args: { label: "Accuracy", value: 3, max: 4, detail: "Same as Sharp" },
};
export default meta;
type Story = StoryObj<MurMeterComponent>;

export const Default: Story = {};
export const WithoutDetail: Story = { args: { label: "Speed", value: 2, detail: null } };

export const ModelComparison: Story = {
  render: () => ({
    template: `
      <div style="display: grid; gap: var(--space-2)">
        <mur-meter label="Accuracy" [value]="4" [max]="4" detail="Best available" />
        <mur-meter label="Speed" [value]="2" [max]="4" />
        <mur-meter label="Battery" [value]="1" [max]="4" detail="Runs the fans" />
      </div>`,
  }),
};
