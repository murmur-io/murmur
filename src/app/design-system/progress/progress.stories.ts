import type { Meta, StoryObj } from "@storybook/angular";

import { MurProgressComponent } from "./progress.component";

const meta: Meta<MurProgressComponent> = {
  title: "Components/Feedback/Progress",
  component: MurProgressComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-progress>` — the ONE linear progress bar. The host is the track (`role=\"progressbar\"`).\n\n" +
          "- **Determinate** — `value` in `0..max`; fill is a width % and `aria-valuenow` is reported.\n" +
          "- **Indeterminate** — `value` is `null` (default); a sliver animates and `aria-valuenow` is omitted, " +
          "which is how a progressbar says “busy, amount unknown”.\n\n" +
          "`sm` (6px) for inline/settings rows, `md` (8px) for the onboarding and privacy wizards. " +
          "Always pass `ariaLabel`.",
      },
    },
  },
  argTypes: {
    value: { control: { type: "range", min: 0, max: 100, step: 1 } },
    max: { control: "number" },
    size: { control: "inline-radio", options: ["sm", "md"] },
    ariaLabel: { control: "text" },
  },
  args: { value: 42, max: 100, size: "sm", ariaLabel: "Model download" },
  render: (args) => ({
    props: args,
    template: `<div style="max-width: 360px"><mur-progress [value]="value" [max]="max" [size]="size" [ariaLabel]="ariaLabel" /></div>`,
  }),
};
export default meta;
type Story = StoryObj<MurProgressComponent>;

export const Determinate: Story = {};
export const Medium: Story = { args: { size: "md", value: 70, ariaLabel: "Setting up Murmur" } };
export const Indeterminate: Story = { args: { value: null, ariaLabel: "Re-indexing" } };
export const Complete: Story = { args: { value: 100 } };
