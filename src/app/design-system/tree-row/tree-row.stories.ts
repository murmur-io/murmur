import { type Meta, type StoryObj, moduleMetadata } from "@storybook/angular";

import { MurRowMenuComponent } from "../row-menu/row-menu.component";
import { MurSidebarComponent } from "../sidebar/sidebar.component";
import { MurTreeRowComponent, type TreeRowIcon } from "./tree-row.component";

/** Keyed by the union so a new glyph that skips this gallery fails to compile. */
const ROW_ICONS: Record<TreeRowIcon, true> = {
  folder: true, locked: true, space: true, meeting: true, note: true, task: true, dashboard: true,
};

const meta: Meta<MurTreeRowComponent> = {
  title: "Components/Navigation/Tree row",
  component: MurTreeRowComponent,
  tags: ["autodocs"],
  decorators: [moduleMetadata({ imports: [MurSidebarComponent, MurRowMenuComponent] })],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-tree-row>` — THE folder-tree row, shared by the Meetings and Notes trees so they " +
          "cannot drift. It owns the whole visual box: padding, radius, hover, the selected pill, the " +
          "per-depth indent (`depth × var(--tree-indent)`), a FIXED leading gutter (caret when " +
          "`expandable`, an equal-width spacer otherwise — so icons align across rows and across both " +
          "trees), the glyph (or an `emoji`), label and an optional `count` chip. Feature actions (lock " +
          "toggle, ⋯ menu) are projected as trailing content.",
      },
    },
  },
  argTypes: {
    label: { control: "text" },
    depth: { control: { type: "range", min: 0, max: 5, step: 1 } },
    selected: { control: "boolean" },
    icon: { control: "select", options: Object.keys(ROW_ICONS) },
    emoji: { control: "text" },
    count: { control: "number" },
    expandable: { control: "boolean" },
    expanded: { control: "boolean" },
    activate: { action: "activate" },
    toggleExpand: { action: "toggleExpand" },
  },
  args: {
    label: "Product",
    depth: 0,
    selected: false,
    icon: "folder",
    emoji: null,
    count: 12,
    expandable: true,
    expanded: false,
  },
  render: (args) => ({
    props: args,
    template: `
      <mur-sidebar style="width: 280px; padding-top: var(--space-3)">
        <mur-tree-row [label]="label" [depth]="depth" [selected]="selected" [icon]="icon" [emoji]="emoji"
          [count]="count" [expandable]="expandable" [expanded]="expanded"
          (activate)="activate()" (toggleExpand)="toggleExpand()" />
      </mur-sidebar>`,
  }),
};
export default meta;
type Story = StoryObj<MurTreeRowComponent>;

export const Folder: Story = {};
export const Selected: Story = { args: { selected: true } };
export const Locked: Story = { args: { icon: "locked", label: "1:1s", count: 4, expandable: false } };
export const WithEmoji: Story = { args: { icon: "space", emoji: "🚀", label: "Launch", count: null } };

/** A nested tree: every depth, leaf and expandable rows, and a projected ⋯ menu. */
export const Tree: Story = {
  parameters: { controls: { disable: true } },
  render: () => ({
    template: `
      <mur-sidebar style="width: 300px; padding-top: var(--space-3)">
        <mur-tree-row label="Acme workspace" icon="space" emoji="🏢" [expandable]="true" [expanded]="true" />
        <mur-tree-row label="Product" [depth]="1" [count]="12" [expandable]="true" [expanded]="true" [selected]="true">
          <mur-row-menu label="Actions for Product">
            <button type="button" class="menu-item" role="menuitem">Rename</button>
            <button type="button" class="menu-item menu-item-danger" role="menuitem">Delete</button>
          </mur-row-menu>
        </mur-tree-row>
        <mur-tree-row label="Roadmap review" icon="meeting" [depth]="2" />
        <mur-tree-row label="Launch checklist" icon="task" [depth]="2" />
        <mur-tree-row label="Pricing notes" icon="note" [depth]="2" />
        <mur-tree-row label="Metrics board" icon="dashboard" [depth]="2" />
        <mur-tree-row label="1:1s" icon="locked" [depth]="1" [count]="4" />
      </mur-sidebar>`,
  }),
};

export const AllIcons: Story = {
  parameters: { controls: { disable: true } },
  render: () => ({
    props: { icons: Object.keys(ROW_ICONS) },
    template: `
      <mur-sidebar style="width: 280px; padding-top: var(--space-3)">
        @for (i of icons; track i) {
          <mur-tree-row [label]="i" [icon]="i" />
        }
      </mur-sidebar>`,
  }),
};
