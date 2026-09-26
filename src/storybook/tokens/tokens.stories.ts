import type { Meta, StoryContext, StoryObj } from "@storybook/angular";

import accents from "../../design-tokens/accents.css?raw";
import colors from "../../design-tokens/colors.css?raw";
import glass from "../../design-tokens/glass.css?raw";
import layout from "../../design-tokens/layout.css?raw";
import skinPaper from "../../design-tokens/skin-paper.css?raw";
import themeLight from "../../design-tokens/theme-light.css?raw";
import typography from "../../design-tokens/typography.css?raw";
import { TokenTableComponent } from "../token-table/token-table.component";

/**
 * One catalogue per file in `src/design-tokens/`. Hidden from the sidebar
 * (`!dev`): each is embedded in its own MDX page under "Design tokens", next
 * to the prose that explains when to use which token.
 */
const meta: Meta<TokenTableComponent> = {
  title: "Design tokens/Catalog",
  component: TokenTableComponent,
  tags: ["!dev", "!autodocs"],
  parameters: { controls: { disable: true }, layout: "padded" },
};
export default meta;
type Story = StoryObj<TokenTableComponent>;

function catalogue(source: string): Story {
  return {
    render: (_args, context: StoryContext) => ({
      // Passing the globals makes the table re-read the live values whenever the
      // Theme / Accent / Skin toolbar changes.
      props: { source, revision: JSON.stringify(context.globals) },
      template: `<app-token-table [source]="source" [revision]="revision" />`,
    }),
  };
}

export const Colors = catalogue(colors);
export const Typography = catalogue(typography);
export const Layout = catalogue(layout);
export const Glass = catalogue(glass);
export const LightTheme = catalogue(themeLight);
export const PaperSkin = catalogue(skinPaper);
export const Accents = catalogue(accents);
