import { type Meta, type StoryObj, moduleMetadata } from "@storybook/angular";

import { MurIconComponent } from "../icon/icon.component";
import { TooltipDirective } from "./tooltip.directive";

interface TooltipArgs {
  text: string;
}

const meta: Meta<TooltipArgs> = {
  title: "Components/Overlays/Tooltip",
  tags: ["autodocs"],
  decorators: [moduleMetadata({ imports: [TooltipDirective, MurIconComponent] })],
  parameters: {
    docs: {
      description: {
        component:
          "`[appTooltip]` — a styled hover/focus tooltip for an icon-only control. It appears after " +
          "350 ms, on keyboard focus too, and hides on click or Escape. It **replaces** `title` (a host " +
          "`title` is stripped and adopted) so the OS never draws a second bubble. It is **not** the " +
          "accessible name: the bubble is `aria-hidden`, so keep the control’s `aria-label`. The bubble " +
          "is created on `<body>` so glass ancestors cannot offset it.",
      },
    },
  },
  argTypes: { text: { control: "text" } },
  args: { text: "Lock all unlocked folders" },
  render: (args) => ({
    props: args,
    template: `
      <div style="display: flex; gap: var(--space-3); padding: var(--space-7)">
        <button type="button" class="btn btn-ghost" [appTooltip]="text" aria-label="Lock all">
          <mur-icon icon="lock" />
        </button>
        <button type="button" class="btn btn-ghost" appTooltip="New note (⌘N)" aria-label="New note">
          <mur-icon icon="note-add" />
        </button>
        <button type="button" class="btn btn-ghost" appTooltip="Move to trash" aria-label="Move to trash">
          <mur-icon icon="trash" />
        </button>
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<TooltipArgs>;

/** Hover (or Tab to) a button to show its tooltip. */
export const IconButtons: Story = {};
