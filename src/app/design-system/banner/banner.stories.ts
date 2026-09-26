import type { Meta, StoryObj } from "@storybook/angular";

import { MurBannerComponent } from "./banner.component";

const meta: Meta<MurBannerComponent> = {
  title: "Components/Feedback/Banner",
  component: MurBannerComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-banner>` — the status strip. Rides the global `.banner` primitive on its host and " +
          "announces itself with `role=\"alert\"`. `warning` and `danger` show `!`, the calm kinds `i`.",
      },
    },
  },
  argTypes: {
    kind: { control: "inline-radio", options: ["info", "success", "warning", "danger"] },
  },
  args: { kind: "info" },
  render: (args) => ({
    props: args,
    template: `<mur-banner [kind]="kind">Transcription finished — the note is ready.</mur-banner>`,
  }),
};
export default meta;
type Story = StoryObj<MurBannerComponent>;

export const Info: Story = {};
export const Success: Story = { args: { kind: "success" } };
export const Warning: Story = { args: { kind: "warning" } };
export const Danger: Story = { args: { kind: "danger" } };

export const AllKinds: Story = {
  render: () => ({
    template: `
      <div style="display: grid; gap: var(--space-3)">
        <mur-banner kind="info">Model download resumes when you’re back online.</mur-banner>
        <mur-banner kind="success">Exported to your Obsidian vault.</mur-banner>
        <mur-banner kind="warning">System audio permission is missing — only your mic is recorded.</mur-banner>
        <mur-banner kind="danger">Couldn’t reach the summarizer. Your transcript is safe.</mur-banner>
      </div>`,
  }),
};
