import type { Meta, StoryObj } from "@storybook/angular";

import { MurRowMenuComponent } from "./row-menu.component";

const meta: Meta<MurRowMenuComponent> = {
  title: "Components/Overlays/Row menu",
  component: MurRowMenuComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-row-menu>` — THE ellipsis-trigger dropdown for per-item actions. It owns only the " +
          "shell: the trigger (quiet and hover-revealed inside a `<mur-tree-row>`, or `prominent` for a " +
          "standalone header), open state, outside-click / Escape dismissal, keyboard entry " +
          "(Enter / Space / ↓) and positioning. The panel is teleported to `<body>` so glass ancestors " +
          "cannot offset it, and paints the OPAQUE global `.menu` primitive (rule T3). Items are " +
          "projected `.menu-item` buttons with `role=\"menuitem\"`; activating one closes the menu.",
      },
    },
  },
  argTypes: {
    label: { control: "text" },
    disabled: { control: "boolean" },
    prominent: { control: "boolean" },
  },
  args: { label: "Actions for Weekly sync", disabled: false, prominent: true },
  render: (args) => ({
    props: args,
    template: `
      <div style="display: flex; align-items: center; gap: var(--space-3); min-height: 220px; align-items: flex-start">
        <h3 style="margin: 0">Weekly sync</h3>
        <mur-row-menu [label]="label" [disabled]="disabled" [prominent]="prominent">
          <div class="menu-group">
            <span class="menu-group-label">Note</span>
            <button type="button" class="menu-item" role="menuitem">Rename</button>
            <button type="button" class="menu-item" role="menuitem">Move to folder…</button>
            <button type="button" class="menu-item" role="menuitem" disabled>Export PDF</button>
          </div>
          <div class="menu-group">
            <button type="button" class="menu-item menu-item-danger" role="menuitem">Move to trash</button>
          </div>
        </mur-row-menu>
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<MurRowMenuComponent>;

export const Prominent: Story = {};
export const Quiet: Story = { args: { prominent: false } };
export const Disabled: Story = { args: { disabled: true } };
