import type { Meta, StoryObj } from "@storybook/angular";

import { MurCardComponent } from "./card.component";

const meta: Meta<MurCardComponent> = {
  title: "Components/Layout/Card",
  component: MurCardComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-card>` — the frosted glass surface for **in-flow** panels (host carries the global " +
          "`.card`). Never use it for anything that floats over content: overlays must be opaque " +
          "`var(--surface-overlay)` (rule T3) or the content behind bleeds through.",
      },
    },
  },
  render: () => ({
    template: `
      <mur-card style="max-width: 420px">
        <h3>Weekly sync</h3>
        <p style="color: var(--text-secondary); margin: 0">
          Decided to ship the Paper skin behind a toggle; Anna owns the contrast audit.
        </p>
      </mur-card>`,
  }),
};
export default meta;
type Story = StoryObj<MurCardComponent>;

export const Default: Story = {};
