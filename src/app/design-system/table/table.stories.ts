import { type Meta, type StoryObj, moduleMetadata } from "@storybook/angular";

import { MurPillComponent } from "../pill/pill.component";
import { MurRowMenuComponent } from "../row-menu/row-menu.component";
import { MurTableColumnComponent } from "./table-column.component";
import { MurTableComponent } from "./table.component";

interface NoteRow {
  id: string;
  title: string;
  folder: string;
  modified: string;
  locked: boolean;
}

const ROWS: NoteRow[] = [
  { id: "n1", title: "Q4 planning — decisions and owners", folder: "Product", modified: "Sep 24", locked: false },
  { id: "n2", title: "Hiring loop debrief", folder: "People", modified: "Sep 22", locked: true },
  {
    id: "n3",
    title: "A very long title that would wrap and make this row taller than its neighbours if the table let it",
    folder: "Research",
    modified: "Sep 19",
    locked: false,
  },
  { id: "n4", title: "Vendor call — Whisper licensing", folder: "Legal", modified: "Sep 12", locked: false },
];

const meta: Meta<MurTableComponent<NoteRow>> = {
  title: "Components/Data display/Table",
  component: MurTableComponent,
  tags: ["autodocs"],
  decorators: [
    moduleMetadata({ imports: [MurTableColumnComponent, MurPillComponent, MurRowMenuComponent] }),
  ],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-table>` — a dense, Notion-style data table. Columns are DEFINITIONS " +
          "(`<mur-table-column key header width alignEnd hideHeader>`), each with an `<ng-template let-row>` " +
          "for rich per-row cell content. `trackBy` is required (never `track $index`). **Row height " +
          "is a design constant this component owns**: long content is clipped, never wrapped, so no " +
          "row can grow taller than another. `rowClass` adds per-row classes (e.g. a masked row).",
      },
    },
  },
  args: { rows: ROWS },
  render: (args) => ({
    props: {
      ...args,
      trackById: (r: NoteRow) => r.id,
      rowClass: (r: NoteRow) => ({ "is-locked": r.locked }),
    },
    template: `
      <mur-table [rows]="rows" [trackBy]="trackById" [rowClass]="rowClass">
        <mur-table-column key="title" header="Title">
          <ng-template let-row>{{ row.locked ? "🔒 Locked" : row.title }}</ng-template>
        </mur-table-column>
        <mur-table-column key="folder" header="Folder" width="140px">
          <ng-template let-row><mur-pill [dot]="false">{{ row.folder }}</mur-pill></ng-template>
        </mur-table-column>
        <mur-table-column key="modified" header="Last modified" width="140px" [alignEnd]="true">
          <ng-template let-row>{{ row.modified }}</ng-template>
        </mur-table-column>
        <mur-table-column key="actions" header="Actions" [hideHeader]="true" width="48px">
          <ng-template let-row>
            <mur-row-menu [label]="'Actions for ' + row.title">
              <button type="button" class="menu-item" role="menuitem">Open</button>
              <button type="button" class="menu-item menu-item-danger" role="menuitem">Move to trash</button>
            </mur-row-menu>
          </ng-template>
        </mur-table-column>
      </mur-table>`,
  }),
};
export default meta;
type Story = StoryObj<MurTableComponent<NoteRow>>;

export const Notes: Story = {};
export const Empty: Story = { args: { rows: [] } };
