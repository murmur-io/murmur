import type { Meta, StoryObj } from "@storybook/angular";

import type { DashboardSummary, NoteCitation, SourceRef } from "../../core/models";
import { SourcePickerComponent } from "./source-picker.component";

const CANDIDATES: NoteCitation[] = [
  { kind: "meeting", id: "m1", title: "Weekly sync", snippet: "Sep 24 · 42 min" },
  { kind: "meeting", id: "m2", title: "Design review — Liquid Glass", snippet: "Sep 18 · 25 min" },
  { kind: "note", id: "n1", title: "Q4 planning — decisions and owners", snippet: "Product" },
  { kind: "note", id: "n2", title: "Pricing notes", snippet: "Research" },
  { kind: "container", id: "f1", title: "Product", snippet: "Folder · 12 items" },
];

const DASHBOARDS: DashboardSummary[] = [
  {
    id: "d1",
    title: "Launch",
    emoji: "🚀",
    tint: null,
    pinned: true,
    position: 0,
    createdAt: "2026-09-01T00:00:00Z",
    updatedAt: "2026-09-20T00:00:00Z",
    tileCount: 4,
    tileKinds: [],
  },
];

/** A prefix-filtered, paginated candidate feed — the shape `list_link_candidates` pages through. */
function listCandidates(args: Record<string, unknown>): NoteCitation[] {
  const prefix = String(args["prefix"] ?? "").toLowerCase();
  const offset = Number(args["offset"] ?? 0);
  const limit = Number(args["limit"] ?? 20);
  return CANDIDATES.filter((c) => c.title.toLowerCase().includes(prefix)).slice(offset, offset + limit);
}

const SELECTED: SourceRef[] = [
  { kind: "meeting", id: "m1", title: "Weekly sync" },
  { kind: "note", id: "n1", title: "Q4 planning — decisions and owners" },
];

const meta: Meta<SourcePickerComponent> = {
  title: "Components/Forms/Source picker",
  component: SourcePickerComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      story: { inline: false, height: "440px" },
      description: {
        component:
          "`<mur-source-picker>` — the “scope the Brain to these sources” chip multiselect. The trigger " +
          "opens an OPAQUE popover (T3, teleported to `<body>`) with a self-focused search over " +
          "`list_link_candidates` — the same paginated feed the `[[` link picker walks — filtered to " +
          "`allowedKinds`. Picking a row adds a `SourceRef` (deduped by kind + id) and keeps the popover " +
          "open; picked sources render as removable chips. Two-way bind `[(selected)]`, and optionally a " +
          "whole board via `[(dashboard)]`. Debounced fetch, stale-reply guard and infinite scroll are " +
          "all inside.",
      },
    },
    tauri: {
      list_link_candidates: listCandidates,
      list_dashboards: () => DASHBOARDS,
      get_dashboard_sources: () => SELECTED,
    },
  },
  argTypes: {
    placeholder: { control: "text" },
    triggerLabel: { control: "text" },
    disabled: { control: "boolean" },
    selectionLimit: { control: "number" },
    allowDashboards: { control: "boolean" },
    dashboardMode: { control: "inline-radio", options: ["expand", "composite"] },
    allowedKinds: {
      control: "check",
      options: ["meeting", "note", "document", "org", "container"],
    },
    selectedChange: { action: "selectedChange" },
    dashboardChange: { action: "dashboardChange" },
  },
  args: {
    selected: [],
    dashboard: null,
    placeholder: "Add a note or meeting…",
    triggerLabel: "+ Source",
    disabled: false,
    selectionLimit: null,
    allowDashboards: true,
    dashboardMode: "expand",
    allowedKinds: ["meeting", "note", "document"],
  },
  render: (args) => ({
    props: args,
    template: `
      <div style="max-width: 520px; min-height: 360px">
        <mur-source-picker [(selected)]="selected" [(dashboard)]="dashboard" [placeholder]="placeholder"
          [triggerLabel]="triggerLabel" [disabled]="disabled" [selectionLimit]="selectionLimit"
          [allowDashboards]="allowDashboards" [dashboardMode]="dashboardMode" [allowedKinds]="allowedKinds"
          (selectedChange)="selectedChange($event)" (dashboardChange)="dashboardChange($event)" />
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<SourcePickerComponent>;

export const Empty: Story = {};
export const WithSelection: Story = { args: { selected: SELECTED } };
export const Limited: Story = { args: { selected: SELECTED, selectionLimit: 2 } };
export const Disabled: Story = { args: { selected: SELECTED, disabled: true } };
export const IncludingFolders: Story = { args: { allowedKinds: ["meeting", "note", "document", "container"] } };
