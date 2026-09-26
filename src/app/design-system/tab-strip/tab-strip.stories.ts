import { signal } from "@angular/core";
import { type Meta, type StoryObj, applicationConfig } from "@storybook/angular";

import { type Tab, TabsService } from "../../core/tabs.service";
import { MurTabStripComponent } from "./tab-strip.component";
import { provideStoryRouter } from "../../../storybook/story-router";

const TABS: Tab[] = [
  { id: "meeting:m1", kind: "meeting", entityId: "m1", title: "Weekly sync", route: ["/meeting", "m1"] },
  { id: "note:n1", kind: "note", entityId: "n1", title: "Q4 planning — decisions", route: ["/notes", "n1"] },
  { id: "note:n2", kind: "note", entityId: "n2", title: "Hiring loop debrief", route: ["/notes", "n2"] },
];

/**
 * A stand-in for `TabsService` carrying only what the strip reads and calls.
 * The real service restores tabs from `localStorage` and drives the router,
 * which would make every story depend on the last one's clicks.
 */
function tabsStub(initial: Tab[]): Partial<TabsService> {
  const tabs = signal<readonly Tab[]>(initial);
  const active = signal<string | null>(initial[0]?.id ?? null);
  return {
    tabs: tabs.asReadonly(),
    activeTabId: active.asReadonly(),
    activate: (id: string) => active.set(id),
    closeTab: async (id: string) => {
      tabs.update((all) => all.filter((t) => t.id !== id));
      if (active() === id) {
        active.set(tabs()[0]?.id ?? null);
      }
    },
  } as Partial<TabsService>;
}

const meta: Meta<MurTabStripComponent> = {
  title: "Components/Navigation/Tab strip",
  component: MurTabStripComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-tab-strip>` — the open-documents strip. It renders `TabsService.tabs()` and nothing " +
          "when there are none; clicking a tab activates it, × closes it (never also activating it), " +
          "and the trailing + routes to `/notes/new` — the same note-creation seam as ⌘N. In these " +
          "stories `TabsService` is a small stub, so clicks work without a router or storage.",
      },
    },
  },
};
export default meta;
type Story = StoryObj<MurTabStripComponent>;

export const OpenTabs: Story = {
  decorators: [
    applicationConfig({
      providers: [...provideStoryRouter(), { provide: TabsService, useValue: tabsStub(TABS) }],
    }),
  ],
};

export const SingleTab: Story = {
  decorators: [
    applicationConfig({
      providers: [...provideStoryRouter(), { provide: TabsService, useValue: tabsStub(TABS.slice(0, 1)) }],
    }),
  ],
};
