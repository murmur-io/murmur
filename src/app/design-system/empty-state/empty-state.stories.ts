import { type Meta, type StoryObj, moduleMetadata } from "@storybook/angular";

import { MurSpinnerComponent } from "../spinner/spinner.component";
import { MurEmptyStateComponent } from "./empty-state.component";

const meta: Meta<MurEmptyStateComponent> = {
  title: "Components/Feedback/Empty state",
  component: MurEmptyStateComponent,
  tags: ["autodocs"],
  decorators: [moduleMetadata({ imports: [MurSpinnerComponent] })],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-empty-state>` — the shared empty/loading well (global `.empty-state`). Compose it " +
          "with the `.empty-mark`, `.empty-title` and `.empty` primitive classes.",
      },
    },
  },
  render: () => ({
    template: `
      <mur-empty-state>
        <div class="empty-mark" aria-hidden="true"></div>
        <p class="empty-title">No meetings yet</p>
        <p class="empty">Hit record and Murmur will transcribe on-device.</p>
        <button type="button" class="btn btn-primary">Start recording</button>
      </mur-empty-state>`,
  }),
};
export default meta;
type Story = StoryObj<MurEmptyStateComponent>;

export const Default: Story = {};

export const Loading: Story = {
  render: () => ({
    template: `
      <mur-empty-state>
        <mur-spinner [size]="24" />
        <p class="empty">Loading meetings…</p>
      </mur-empty-state>`,
  }),
};
