import type { Meta, StoryObj } from "@storybook/angular";

import type { AskConversationSummary } from "../../core/models";
import { ChatHistoryComponent } from "./chat-history.component";

const THREADS: AskConversationSummary[] = [
  {
    id: "t1",
    scope: { kind: "vault" },
    title: "What did we decide about the Paper skin?",
    createdAt: "2026-09-24T09:12:00Z",
    updatedAt: "2026-09-24T09:40:00Z",
    messageCount: 6,
  },
  {
    id: "t2",
    scope: { kind: "meeting", refId: "m42" },
    title: "Action items from the hiring debrief",
    createdAt: "2026-09-22T14:02:00Z",
    updatedAt: "2026-09-22T14:05:00Z",
    messageCount: 2,
  },
  {
    id: "t3",
    scope: { kind: "note", refId: "n7" },
    title: "Summarise the Whisper licensing thread for legal",
    createdAt: "2026-09-19T08:30:00Z",
    updatedAt: "2026-09-20T11:15:00Z",
    messageCount: 9,
  },
];

const meta: Meta<ChatHistoryComponent> = {
  title: "Components/Data display/Chat history",
  component: ChatHistoryComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-chat-history>` — the in-flow browser for durable Ask Brain conversations. The owning " +
          "Ask surface owns IPC and view state; this primitive only renders rows and emits stable ids " +
          "(`selected`, `retryRequested`). States: first load, load error, empty, list, refreshing " +
          "(cached rows stay visible — stale-while-revalidate), and a non-destructive `notice`. No card " +
          "frame on purpose, so a narrow drawer stays one pane instead of glass nested in glass.",
      },
    },
  },
  argTypes: {
    loading: { control: "boolean" },
    error: { control: "text" },
    notice: { control: "text" },
    activeThreadId: { control: "inline-radio", options: [null, "t1", "t2", "t3"] },
    loadingThreadId: { control: "inline-radio", options: [null, "t1", "t2", "t3"] },
    selected: { action: "selected" },
    retryRequested: { action: "retryRequested" },
  },
  args: {
    threads: THREADS,
    loading: false,
    error: null,
    notice: null,
    activeThreadId: "t1",
    loadingThreadId: null,
  },
  render: (args) => ({
    props: args,
    template: `
      <div style="max-width: 360px">
        <mur-chat-history [threads]="threads" [loading]="loading" [error]="error" [notice]="notice"
          [activeThreadId]="activeThreadId" [loadingThreadId]="loadingThreadId"
          (selected)="selected($event)" (retryRequested)="retryRequested()" />
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<ChatHistoryComponent>;

export const List: Story = {};
export const FirstLoad: Story = { args: { threads: [], loading: true } };
export const Refreshing: Story = { args: { loading: true } };
export const Empty: Story = { args: { threads: [] } };
export const LoadError: Story = { args: { threads: [], error: "Couldn’t load conversations — the database is locked." } };
export const OpeningThread: Story = { args: { loadingThreadId: "t2" } };
export const WithNotice: Story = { args: { notice: "Couldn’t delete that conversation. It’s still here." } };
