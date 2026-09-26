import type { StorybookConfig } from "@storybook/angular";

/**
 * Storybook for Murmur's design system (`src/app/design-system`) and its
 * tokens (`src/design-tokens`).
 *
 * Run it through the Angular CLI targets in `angular.json`
 * (`npm run storybook` / `npm run build-storybook`), never `storybook dev`
 * directly: the target is what hands Storybook the app's global stylesheet,
 * the self-hosted fonts and `experimentalZoneless`.
 */
const config: StorybookConfig = {
  stories: ["../src/**/*.mdx", "../src/**/*.stories.ts"],
  addons: ["@storybook/addon-docs"],
  framework: {
    name: "@storybook/angular",
    options: {},
  },
  core: {
    disableTelemetry: true,
  },
  webpackFinal: async (webpackConfig) => {
    // `import css from "…/colors.css?raw"` hands the token pages the SOURCE of a
    // token file (names, values and the comments that explain them), so the docs
    // are read from the file itself and can never drift from it. Every other
    // CSS rule is told to skip `?raw`, or the file would also go through the
    // style pipeline and come back as a module instead of a string.
    const rules = webpackConfig.module?.rules ?? [];
    for (const rule of rules) {
      if (rule && typeof rule === "object" && !rule.resourceQuery) {
        rule.resourceQuery = { not: [/raw/] };
      }
    }
    rules.unshift({ resourceQuery: /raw/, type: "asset/source" });
    return webpackConfig;
  },
};

export default config;
