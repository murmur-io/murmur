import type { Meta, StoryObj } from "@storybook/angular";

import { meetingStatusLabel, meetingStatusPillClass } from "../meeting-status";
import { MurPillComponent } from "./pill.component";

const meta: Meta<MurPillComponent> = {
  title: "Components/Data display/Pill",
  component: MurPillComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-pill>` — the status pill (global `.pill` primitive). `dot` toggles the leading " +
          "status dot. Meeting rows map a backend status to a pill with `meetingStatusPillClass()` " +
          "/ `meetingStatusLabel()` from `design-system/meeting-status.ts`.",
      },
    },
  },
  argTypes: {
    kind: {
      control: "inline-radio",
      options: ["neutral", "success", "warning", "danger", "accent"],
    },
    dot: { control: "boolean" },
  },
  args: { kind: "neutral", dot: true },
  render: (args) => ({
    props: args,
    template: `<mur-pill [kind]="kind" [dot]="dot">Summarized</mur-pill>`,
  }),
};
export default meta;
type Story = StoryObj<MurPillComponent>;

export const Neutral: Story = {};
export const Accent: Story = { args: { kind: "accent" } };
export const WithoutDot: Story = { args: { kind: "success", dot: false } };

export const AllKinds: Story = {
  render: () => ({
    template: `
      <div style="display: flex; flex-wrap: wrap; gap: var(--space-2)">
        <mur-pill>Neutral</mur-pill>
        <mur-pill kind="accent">Accent</mur-pill>
        <mur-pill kind="success">Success</mur-pill>
        <mur-pill kind="warning">Warning</mur-pill>
        <mur-pill kind="danger">Danger</mur-pill>
      </div>`,
  }),
};

const STATUSES = ["RECORDING", "QUEUED", "TRANSCRIBED", "SUMMARIZED", "EXPORTED", "ERROR"];

/** Every meeting status through the shared helpers — the exact mapping list rows use. */
export const MeetingStatuses: Story = {
  render: () => ({
    props: {
      rows: STATUSES.map((s) => ({
        status: s,
        cls: `pill ${meetingStatusPillClass(s)}`,
        label: meetingStatusLabel(s),
      })),
    },
    template: `
      <div style="display: flex; flex-wrap: wrap; gap: var(--space-2)">
        @for (r of rows; track r.status) {
          <span [class]="r.cls"><span class="pill-dot" aria-hidden="true"></span>{{ r.label }}</span>
        }
      </div>`,
  }),
};
