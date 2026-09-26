import type { Meta, StoryObj } from "@storybook/angular";

import { MurKbdComponent } from "./kbd.component";

const meta: Meta<MurKbdComponent> = {
  title: "Components/Data display/Kbd",
  component: MurKbdComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: { component: "`<mur-kbd>` — a keyboard-shortcut chip (⌘K, esc…). Content is projected." },
    },
  },
  render: () => ({ template: `<mur-kbd>⌘K</mur-kbd>` }),
};
export default meta;
type Story = StoryObj<MurKbdComponent>;

export const Default: Story = {};

export const InCopy: Story = {
  render: () => ({
    template: `
      <p style="color: var(--text-secondary); margin: 0">
        Press <mur-kbd>⌘</mur-kbd> <mur-kbd>K</mur-kbd> to search, <mur-kbd>↑</mur-kbd> <mur-kbd>↓</mur-kbd>
        to move and <mur-kbd>esc</mur-kbd> to close.
      </p>`,
  }),
};
