import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { type Meta, type StoryObj, moduleMetadata } from "@storybook/angular";

import { MurInputComponent } from "./input.component";

const meta: Meta<MurInputComponent> = {
  title: "Components/Forms/Input",
  component: MurInputComponent,
  tags: ["autodocs"],
  decorators: [moduleMetadata({ imports: [ReactiveFormsModule] })],
  parameters: {
    docs: {
      description: {
        component:
          "`<mur-input>` — a text field as a form control (ControlValueAccessor), so " +
          "`formControlName` / `[formControl]` bind directly. Visual language comes from the global " +
          "field primitives. For **number** fields keep the native `<input type=\"number\">`: " +
          "Angular's `NumberValueAccessor` commits numbers, this CVA commits strings.",
      },
    },
  },
  argTypes: {
    type: { control: "inline-radio", options: ["text", "password", "search", "url", "email"] },
    placeholder: { control: "text" },
    ariaLabel: { control: "text" },
    autocomplete: { control: "text" },
    spellcheck: { control: "boolean" },
  },
  args: {
    type: "text",
    placeholder: "Meeting title",
    ariaLabel: "Meeting title",
    autocomplete: "off",
    spellcheck: false,
  },
  render: (args) => ({
    props: { ...args, control: new FormControl("") },
    template: `
      <label style="display: grid; gap: var(--space-2); max-width: 360px; color: var(--text-secondary)">
        Title
        <mur-input [formControl]="control" [type]="type" [placeholder]="placeholder"
          [ariaLabel]="ariaLabel" [autocomplete]="autocomplete" [spellcheck]="spellcheck" />
      </label>
      <p style="color: var(--text-tertiary); font-size: 12px">Form value: “{{ control.value }}”</p>`,
  }),
};
export default meta;
type Story = StoryObj<MurInputComponent>;

export const Text: Story = {};
export const Password: Story = { args: { type: "password", placeholder: "API key", ariaLabel: "API key" } };
export const Url: Story = { args: { type: "url", placeholder: "http://127.0.0.1:11434", ariaLabel: "Ollama URL" } };

export const Disabled: Story = {
  render: (args) => ({
    props: { ...args, control: new FormControl({ value: "Weekly sync", disabled: true }) },
    template: `<div style="max-width: 360px"><mur-input [formControl]="control" [ariaLabel]="ariaLabel" /></div>`,
  }),
};
