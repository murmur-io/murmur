import type { Meta, StoryObj } from "@storybook/angular";

import { MurIconComponent, type ShellIcon } from "./icon.component";

/**
 * Every glyph, keyed by the `ShellIcon` union. A `Record` rather than an array
 * so the compiler fails this file the moment a glyph is added to (or removed
 * from) the union without the gallery following.
 */
const GLYPHS: Record<ShellIcon, true> = {
  murmur: true, record: true, meetings: true, notes: true, reminders: true, tasks: true,
  dashboards: true, analytics: true, graph: true, people: true, brain: true, ask: true,
  settings: true, search: true, spaces: true, "shared-brains": true, browse: true,
  history: true, plus: true, "note-add": true, folder: true, "folder-add": true, move: true,
  rename: true, edit: true, eye: true, trash: true, unlock: true, check: true,
  "chevron-right": true, sidebar: true, topbar: true, sun: true, moon: true, display: true,
  document: true, drift: true, numbers: true, pulse: true, promises: true, lock: true,
  developer: true, logs: true, link: true, close: true,
};
const ICONS = Object.keys(GLYPHS) as ShellIcon[];

const meta: Meta<MurIconComponent> = {
  title: "Components/Data display/Icon",
  component: MurIconComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-icon>` — one inline-SVG glyph from the shell icon set (no icon font, no package). " +
          "Glyphs draw in `currentColor`, so they take the colour of their context. The set lives in " +
          "exactly one place: add a glyph to the `ShellIcon` union and its `@case`, never inline an " +
          "SVG in a feature.",
      },
    },
  },
  argTypes: { icon: { control: "select", options: ICONS } },
  args: { icon: "murmur" },
};
export default meta;
type Story = StoryObj<MurIconComponent>;

export const Default: Story = {};

export const Gallery: Story = {
  parameters: { controls: { disable: true } },
  render: () => ({
    props: { icons: ICONS },
    template: `
      <div style="display: grid; grid-template-columns: repeat(auto-fill, minmax(112px, 1fr)); gap: var(--space-2)">
        @for (name of icons; track name) {
          <div style="display: flex; flex-direction: column; align-items: center; gap: var(--space-2);
                      padding: var(--space-3); border: 1px solid var(--border-subtle);
                      border-radius: var(--radius-md); color: var(--text-primary)">
            <mur-icon [icon]="name" />
            <code style="font-size: 11px; color: var(--text-tertiary)">{{ name }}</code>
          </div>
        }
      </div>`,
  }),
};

export const Tinted: Story = {
  parameters: { controls: { disable: true } },
  render: () => ({
    template: `
      <div style="display: flex; gap: var(--space-4)">
        <span style="color: var(--accent)"><mur-icon icon="record" /></span>
        <span style="color: var(--live)"><mur-icon icon="record" /></span>
        <span style="color: var(--danger)"><mur-icon icon="trash" /></span>
        <span style="color: var(--text-tertiary)"><mur-icon icon="lock" /></span>
      </div>`,
  }),
};
