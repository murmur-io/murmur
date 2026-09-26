import type { Meta, StoryObj } from "@storybook/angular";

import type { ModelCatalog } from "../../core/models";
import { MurModelPickerComponent } from "./model-picker.component";

const LIVE: ModelCatalog = {
  source: "live",
  options: [
    { id: "llama3.2:3b", label: "Llama 3.2 · 3B", source: "live" },
    { id: "qwen2.5:7b", label: "Qwen 2.5 · 7B", source: "live" },
    { id: "gemma3:12b", label: "Gemma 3 · 12B", source: "live" },
  ],
};

const BUNDLED: ModelCatalog = {
  source: "bundled",
  options: [
    { id: "claude-sonnet-5", label: "Claude Sonnet 5", source: "bundled" },
    { id: "claude-opus-5-5", label: "Claude Opus 5.5", source: "bundled" },
  ],
};

const meta: Meta<MurModelPickerComponent> = {
  title: "Components/Forms/Model picker",
  component: MurModelPickerComponent,
  tags: ["autodocs"],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-model-picker>` — THE model picker (it used to exist three times). It owns:\n\n" +
          "- options render their **label**, never a raw id;\n" +
          "- the free-text id is **always** available — a bundled catalog is only a hint, and a model " +
          "released after this build must still be selectable;\n" +
          "- a value missing from the catalog is kept and shown as *custom*, never cleared;\n" +
          "- Refresh shows unless the catalog is known-bundled (`canRefresh`);\n" +
          "- a bundled catalog says so instead of staying silent.\n\n" +
          "One ControlValueAccessor over both the select and the text field, so they always agree.",
      },
    },
  },
  argTypes: {
    value: { control: "text" },
    loading: { control: "boolean" },
    disabled: { control: "boolean" },
    canRefresh: { control: "boolean" },
    defaultLabel: { control: "text" },
    placeholder: { control: "text" },
    ariaLabel: { control: "text" },
    refresh: { action: "refresh" },
    modelEdited: { action: "modelEdited" },
    valueChange: { action: "valueChange" },
  },
  args: {
    catalog: LIVE,
    value: "qwen2.5:7b",
    loading: false,
    disabled: false,
    canRefresh: true,
    defaultLabel: "Default (provider's pick)",
    placeholder: "Model id (blank = default)",
    ariaLabel: "Model",
  },
  render: (args) => ({
    props: args,
    template: `
      <div style="max-width: 520px">
        <mur-model-picker [catalog]="catalog" [(value)]="value" [loading]="loading" [disabled]="disabled"
          [canRefresh]="canRefresh" [ariaLabel]="ariaLabel"
          [defaultLabel]="defaultLabel" [placeholder]="placeholder"
          (refresh)="refresh()" (modelEdited)="modelEdited()" (valueChange)="valueChange($event)" />
      </div>`,
  }),
};
export default meta;
type Story = StoryObj<MurModelPickerComponent>;

/** A live catalog from Ollama/Gateway — Refresh is offered. */
export const LiveCatalog: Story = {};
export const Refreshing: Story = { args: { loading: true } };

/** A catalog that ships with the app — no Refresh, and the notice explains why. */
export const BundledCatalog: Story = {
  args: { catalog: BUNDLED, value: "claude-sonnet-5", canRefresh: false },
};

/** A stored id the catalog does not list is kept and marked custom. */
export const CustomValue: Story = { args: { value: "mistral-small:24b" } };

/** No catalog at all (a failed fetch): only the free-text id, plus the retry. */
export const NoCatalog: Story = { args: { catalog: undefined, value: "" } };
