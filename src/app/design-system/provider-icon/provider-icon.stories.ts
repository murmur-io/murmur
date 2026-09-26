import type { Meta, StoryObj } from "@storybook/angular";

import { MurProviderIconComponent } from "./provider-icon.component";

const PROVIDERS = ["claude_code", "codex_cli", "anthropic", "ollama", "gateway", "unknown"];

const meta: Meta<MurProviderIconComponent> = {
  title: "Components/Data display/Provider icon",
  component: MurProviderIconComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-provider-icon>` — the compact provider mark for AI engine rows. App-native, " +
          "single-colour paths (never remote assets), so they follow the theme. An unknown provider " +
          "id falls back to a neutral spark.",
      },
    },
  },
  argTypes: { provider: { control: "select", options: PROVIDERS } },
  args: { provider: "claude_code" },
};
export default meta;
type Story = StoryObj<MurProviderIconComponent>;

export const Default: Story = {};

export const AllProviders: Story = {
  parameters: { controls: { disable: true } },
  render: () => ({
    props: { providers: PROVIDERS },
    template: `
      <div style="display: flex; flex-wrap: wrap; gap: var(--space-4)">
        @for (p of providers; track p) {
          <div style="display: flex; align-items: center; gap: var(--space-2); color: var(--text-primary)">
            <mur-provider-icon [provider]="p" />
            <code style="font-size: 12px; color: var(--text-tertiary)">{{ p }}</code>
          </div>
        }
      </div>`,
  }),
};
