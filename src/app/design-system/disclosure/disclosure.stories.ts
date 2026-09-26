import type { Meta, StoryObj } from "@storybook/angular";

import { MurDisclosureComponent } from "./disclosure.component";

const meta: Meta<MurDisclosureComponent> = {
  title: "Components/Layout/Disclosure",
  component: MurDisclosureComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-disclosure>` — the progressive-disclosure section (WAI-ARIA Disclosure). The trigger " +
          "is a real `<button>` with `aria-expanded` + `aria-controls`; the panel stays in the DOM and " +
          "toggles via `hidden`, so `aria-controls` never dangles. Two-way bind `[(open)]`. Project the " +
          "summary with the `murDisclosureSummary` attribute. The host is `display: contents` on " +
          "purpose — it must not add a box between a flex parent and the panel.",
      },
    },
  },
  argTypes: {
    open: { control: "boolean" },
    panelLabel: { control: "text" },
    openChange: { action: "openChange" },
  },
  args: { open: false, panelLabel: "Advanced settings" },
  render: (args) => ({
    props: args,
    template: `
      <div style="display: flex; flex-direction: column; gap: var(--space-3); max-width: 480px">
        <mur-disclosure [(open)]="open" [panelLabel]="panelLabel" (openChange)="openChange($event)">
          <span murDisclosureSummary>⚙ Advanced</span>
          <p style="margin: 0; color: var(--text-secondary)">
            Beam size, temperature fallback and the VAD threshold live here.
          </p>
        </mur-disclosure>
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<MurDisclosureComponent>;

export const Collapsed: Story = {};
export const Expanded: Story = { args: { open: true } };
