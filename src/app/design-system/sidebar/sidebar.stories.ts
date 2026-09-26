import { type Meta, type StoryObj, moduleMetadata } from "@storybook/angular";

import { MurIconComponent } from "../icon/icon.component";
import { MurSidebarComponent } from "./sidebar.component";

const meta: Meta<MurSidebarComponent> = {
  title: "Components/Layout/Sidebar",
  component: MurSidebarComponent,
  tags: ["autodocs"],
  decorators: [moduleMetadata({ imports: [MurIconComponent] })],
  parameters: {
    layout: "fullscreen",
    docs: {
      description: {
        component:
          "`<mur-sidebar>` — THE floating Liquid Glass rail. One look for every rail: the primary " +
          "sidebar, Settings sections and the Meetings folder tree all project into it. The glass " +
          "comes from the GLOBAL `.drill-rail` class (the shell must be styleable before Angular " +
          "boots — the WKWebView cold-launch FOUC fix), and the row shape from the `--sidebar-*` " +
          "tokens shared with `<mur-tree-row>`. Glass is chrome, never content.",
      },
    },
  },
  render: () => ({
    template: `
      <div style="height: 560px; display: flex">
        <mur-sidebar class="app-sidebar primary-sidebar" role="navigation" aria-label="Primary navigation"
          style="width: 248px; padding-top: var(--space-4)">
          <button type="button" class="sb-row">
            <span class="sb-caret sb-caret--leaf" aria-hidden="true"></span>
            <mur-icon icon="search" /><span class="sb-label">Search</span>
          </button>
          <a class="sb-row active" aria-current="page">
            <span class="sb-caret sb-caret--leaf" aria-hidden="true"></span>
            <mur-icon icon="ask" /><span class="sb-label">Ask</span>
          </a>
          <a class="sb-row">
            <span class="sb-caret sb-caret--leaf" aria-hidden="true"></span>
            <mur-icon icon="meetings" /><span class="sb-label">Meetings</span>
          </a>
          <a class="sb-row">
            <span class="sb-caret sb-caret--leaf" aria-hidden="true"></span>
            <mur-icon icon="notes" /><span class="sb-label">Notes</span>
          </a>
          <a class="sb-row">
            <span class="sb-caret sb-caret--leaf" aria-hidden="true"></span>
            <mur-icon icon="reminders" /><span class="sb-label">Reminders</span>
            <span class="count">3</span>
          </a>
          <a class="sb-row">
            <span class="sb-caret sb-caret--leaf" aria-hidden="true"></span>
            <mur-icon icon="spaces" /><span class="sb-label">Workspaces</span>
          </a>
          <div style="flex: 1"></div>
          <a class="sb-row">
            <span class="sb-caret sb-caret--leaf" aria-hidden="true"></span>
            <mur-icon icon="settings" /><span class="sb-label">Settings</span>
          </a>
        </mur-sidebar>
        <main style="flex: 1; padding: var(--space-6); color: var(--text-secondary)">
          The content pane sits beside the rail; the aurora behind both is what the glass blurs.
        </main>
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<MurSidebarComponent>;

export const PrimaryRail: Story = {};
