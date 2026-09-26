import type { Meta, StoryObj } from "@storybook/angular";

import { MurSpinnerComponent } from "./spinner.component";

const meta: Meta<MurSpinnerComponent> = {
  title: "Components/Feedback/Spinner",
  component: MurSpinnerComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-spinner>` — the ring spinner (`role=\"status\"`, labelled “Loading”). `size` is the diameter in px.",
      },
    },
  },
  argTypes: { size: { control: { type: "range", min: 10, max: 64, step: 2 } } },
  args: { size: 16 },
};
export default meta;
type Story = StoryObj<MurSpinnerComponent>;

export const Default: Story = {};
export const Large: Story = { args: { size: 40 } };

export const Sizes: Story = {
  render: () => ({
    template: `
      <div style="display: flex; align-items: center; gap: var(--space-4)">
        <mur-spinner [size]="12" /><mur-spinner [size]="16" /><mur-spinner [size]="24" /><mur-spinner [size]="40" />
      </div>`,
  }),
};
