import { type Meta, type StoryObj, applicationConfig, moduleMetadata } from "@storybook/angular";

import { MurSidebarComponent } from "../sidebar/sidebar.component";
import { MurTreeRowComponent } from "../tree-row/tree-row.component";
import { MurSidebarSectionComponent } from "./sidebar-section.component";
import { provideStoryRouter } from "../../../storybook/story-router";

const meta: Meta<MurSidebarSectionComponent> = {
  title: "Components/Navigation/Sidebar section",
  component: MurSidebarSectionComponent,
  tags: ["autodocs"],
  decorators: [
    applicationConfig({ providers: [provideStoryRouter()] }),
    moduleMetadata({ imports: [MurSidebarComponent, MurTreeRowComponent] }),
  ],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-sidebar-section>` — the expandable nav header shared by Meetings and Notes: a " +
          "`routerLink` row (icon + label), a “new folder” button and a disclosure caret. The folder " +
          "tree below it is projected by the feature. With `enableHeaderDrop` the header doubles as the " +
          "vault-root drop target (dropping a note there files it back to “no folder”).",
      },
    },
  },
  argTypes: {
    icon: { control: "select", options: ["meetings", "notes", "tasks", "dashboards"] },
    label: { control: "text" },
    routePath: { control: "text" },
    addFolderLabel: { control: "text" },
    expanded: { control: "boolean" },
    enableHeaderDrop: { control: "boolean" },
    toggleExpanded: { action: "toggleExpanded" },
    addFolder: { action: "addFolder" },
    headerSelect: { action: "headerSelect" },
    headerDropNote: { action: "headerDropNote" },
  },
  args: {
    routePath: "/meetings",
    icon: "meetings",
    label: "Meetings",
    addFolderLabel: "New meetings folder",
    expanded: true,
    enableHeaderDrop: false,
  },
  render: (args) => ({
    props: args,
    template: `
      <mur-sidebar class="app-sidebar" style="width: 260px; padding-top: var(--space-3)">
        <mur-sidebar-section [routePath]="routePath" [icon]="icon" [label]="label"
          [addFolderLabel]="addFolderLabel" [expanded]="expanded" [enableHeaderDrop]="enableHeaderDrop"
          (toggleExpanded)="toggleExpanded()" (addFolder)="addFolder()"
          (headerSelect)="headerSelect()" (headerDropNote)="headerDropNote($event)">
          @if (expanded) {
            <mur-tree-row label="Product" [depth]="1" [count]="12" />
            <mur-tree-row label="1:1s" icon="locked" [depth]="1" [count]="4" />
          }
        </mur-sidebar-section>
      </mur-sidebar>`,
  }),
};
export default meta;
type Story = StoryObj<MurSidebarSectionComponent>;

export const Expanded: Story = {};
export const Collapsed: Story = { args: { expanded: false } };
export const Notes: Story = {
  args: { routePath: "/notes", icon: "notes", label: "Notes", addFolderLabel: "New notes folder" },
};
