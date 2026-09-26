import { type Meta, type StoryObj, applicationConfig } from "@storybook/angular";

import type { AskVaultResult, SearchHit } from "../../core/models";
import { TabsService } from "../../core/tabs.service";
import { MurQuickSearchComponent } from "./quick-search.component";
import { provideStoryRouter } from "../../../storybook/story-router";

const HITS: SearchHit[] = [
  {
    meeting: {
      id: "m1",
      startedAt: "2026-09-24T09:00:00Z",
      endedAt: "2026-09-24T09:42:00Z",
      title: "Weekly sync",
      durationS: 2520,
      audioPath: null,
      status: "EXPORTED",
    },
    snippet: "…we agreed the Paper skin ships behind a toggle, and Anna owns the contrast audit…",
    matchedIn: "transcript",
  },
  {
    meeting: {
      id: "m2",
      startedAt: "2026-09-18T14:00:00Z",
      endedAt: "2026-09-18T14:25:00Z",
      title: "Design review — Liquid Glass",
      durationS: 1500,
      audioPath: null,
      status: "SUMMARIZED",
    },
    snippet: "Decision: glass is chrome, never content; overlays stay opaque.",
    matchedIn: "note",
  },
];

const ANSWER: AskVaultResult = {
  answer:
    "You decided to ship the **Paper** skin behind a toggle in Settings → Appearance. " +
    "Anna owns the contrast audit before it becomes a default.",
  sources: [{ meetingId: "m1", title: "Weekly sync", startedAt: "2026-09-24T09:00:00Z" }],
  citations: [],
};

const meta: Meta<MurQuickSearchComponent> = {
  title: "Components/Navigation/Quick search",
  component: MurQuickSearchComponent,
  tags: ["autodocs"],
  decorators: [
    applicationConfig({
      providers: [
        ...provideStoryRouter(),
        // Opening a hit calls TabsService.openMeeting; the real one drives the router + storage.
        { provide: TabsService, useValue: { openMeeting: async () => undefined } },
      ],
    }),
  ],
  parameters: {
    layout: "fullscreen",
    docs: {
      story: { inline: false, height: "560px" },
      description: {
        component:
          "`<mur-quick-search>` — the ⌘K palette. Two modes: **Search** (debounced `search_meetings`, " +
          "↑/↓ to move, Enter to open in a tab, Enter on an empty query creates a note) and **Ask Brain** " +
          "(`ask_vault`, answer rendered as markdown with its sources). Stale responses are dropped " +
          "(`switchMap` + a request sequence), and clicking the scrim closes it (`closed`).\n\n" +
          "In Storybook the Tauri bridge is mocked: type anything to get the sample hits, or switch to " +
          "Ask Brain and press Enter.",
      },
    },
    tauri: {
      search_meetings: () => HITS,
      ask_vault: () => ANSWER,
    },
  },
  argTypes: { closed: { action: "closed" } },
  render: (args) => ({
    props: args,
    template: `<mur-quick-search (closed)="closed()" />`,
  }),
};
export default meta;
type Story = StoryObj<MurQuickSearchComponent>;

export const Palette: Story = {};

export const NoResults: Story = {
  parameters: { tauri: { search_meetings: () => [] } },
};

export const BrainError: Story = {
  parameters: {
    tauri: {
      search_meetings: () => HITS,
      ask_vault: () => {
        throw new Error("The summarizer is offline");
      },
    },
  },
};
