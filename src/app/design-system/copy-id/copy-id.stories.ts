import type { Meta, StoryObj } from "@storybook/angular";

import { MurCopyIdComponent } from "./copy-id.component";

const meta: Meta<MurCopyIdComponent> = {
  title: "Components/Actions/Copy ID",
  component: MurCopyIdComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-copy-id>` — the minimal “copy this thing’s id” affordance for a header. The local " +
          "MCP server addresses everything by id (`get_meeting`, `get_document`…), so this is how a " +
          "user points Claude at one exact item. Copies the id verbatim via `CopyIdService`, swaps " +
          "to a tick for 1.6 s and raises a toast. `label` names the kind in the tooltip and toast.",
      },
    },
  },
  argTypes: {
    id: { control: "text" },
    label: { control: "inline-radio", options: ["Meeting", "Note", "Board", "Task"] },
  },
  args: { id: "mtg_01J9ZK3Q7R4M2", label: "Meeting" },
  render: (args) => ({
    props: args,
    template: `
      <div style="display: flex; align-items: center; gap: var(--space-2); font-size: 12px; color: var(--text-tertiary)">
        <span>Sep 24 · 42 min</span>
        <mur-copy-id [id]="id" [label]="label" />
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<MurCopyIdComponent>;

export const Default: Story = {};
export const Note: Story = { args: { id: "note_7F2C", label: "Note" } };
