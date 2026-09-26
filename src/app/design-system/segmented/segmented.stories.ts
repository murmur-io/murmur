import type { Meta, StoryObj } from "@storybook/angular";

import { MurSegmentedComponent, type SegmentOption } from "./segmented.component";

const THEME_OPTIONS: readonly SegmentOption[] = [
  { value: "light", label: "Light", icon: "sun" },
  { value: "dark", label: "Dark", icon: "moon" },
  { value: "system", label: "System", icon: "display" },
];

const RANGE_OPTIONS: readonly SegmentOption[] = [
  { value: "7d", label: "7 days" },
  { value: "30d", label: "30 days" },
  { value: "90d", label: "90 days" },
  { value: "all", label: "All time" },
];

const meta: Meta<MurSegmentedComponent> = {
  title: "Components/Forms/Segmented",
  component: MurSegmentedComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-segmented>` — the pill segmented control (the Light / Dark / System pattern). " +
          "`options` is a list of `SegmentOption { value, label, icon? }`; two-way bind the " +
          "selection with `[(value)]`. Buttons expose `aria-pressed`.",
      },
    },
  },
  argTypes: {
    value: { control: "text" },
    ariaLabel: { control: "text" },
    valueChange: { action: "valueChange" },
  },
  args: { options: THEME_OPTIONS, value: "dark", ariaLabel: "Theme" },
  render: (args) => ({
    props: args,
    template: `<mur-segmented [options]="options" [(value)]="value" [ariaLabel]="ariaLabel" (valueChange)="valueChange($event)" />`,
  }),
};
export default meta;
type Story = StoryObj<MurSegmentedComponent>;

export const WithIcons: Story = {};
export const TextOnly: Story = { args: { options: RANGE_OPTIONS, value: "30d", ariaLabel: "Range" } };
