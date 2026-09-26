import type { Meta, StoryObj } from "@storybook/angular";

import { MurOrgBrainCtaComponent } from "./org-brain-cta.component";

const meta: Meta<MurOrgBrainCtaComponent> = {
  title: "Components/Actions/Org Brain CTA",
  component: MurOrgBrainCtaComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-org-brain-cta>` — the prominent “Add to Org Brain” card at the top of the note / " +
          "meeting share surfaces. Purely presentational: it emits `add` and the host opens the org " +
          "share sheet, where the user picks the organization (so the card names none). `shared` " +
          "flips it to the calm “In Org Brain ✓ / Manage” state.",
      },
    },
  },
  argTypes: {
    disabled: { control: "boolean" },
    shared: { control: "boolean" },
    add: { action: "add" },
  },
  args: { disabled: false, shared: false },
  render: (args) => ({
    props: args,
    template: `<div style="max-width: 420px"><mur-org-brain-cta [disabled]="disabled" [shared]="shared" (add)="add()" /></div>`,
  }),
};
export default meta;
type Story = StoryObj<MurOrgBrainCtaComponent>;

export const NotShared: Story = {};
export const Shared: Story = { args: { shared: true } };
export const Disabled: Story = { args: { disabled: true } };
