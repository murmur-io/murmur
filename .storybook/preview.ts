import { DocsContainer, type DocsContainerProps } from "@storybook/addon-docs/blocks";
import type { Decorator, Preview } from "@storybook/angular";
import { type PropsWithChildren, createElement } from "react";
import { themes } from "storybook/theming";

import { type ThemeGlobal, isLightTheme } from "./theme-global";
import { installTauriMock, setTauriHandlers, type TauriHandlers } from "./tauri-mock";

installTauriMock();

/**
 * Mirror the app's three appearance axes onto <html>, exactly as
 * `ThemeService` / `ChromeService` do, so every token block in
 * `src/design-tokens/*.css` activates the same way it does in Murmur:
 *   - `data-theme`  — light / dark / system (always stamped, like ThemeService)
 *   - `data-accent` — absent for the default purple
 *   - `data-skin`   — absent for the default "Studio" skin
 */
const withAppearance: Decorator = (story, context) => {
  const root = document.documentElement;
  const theme = (context.globals["theme"] as ThemeGlobal | undefined) ?? "dark";
  const accent = (context.globals["accent"] as string | undefined) ?? "purple";
  const skin = (context.globals["skin"] as string | undefined) ?? "studio";

  root.setAttribute("data-theme", theme);
  if (accent === "purple") {
    root.removeAttribute("data-accent");
  } else {
    root.setAttribute("data-accent", accent);
  }
  if (skin === "studio") {
    root.removeAttribute("data-skin");
  } else {
    root.setAttribute("data-skin", skin);
  }
  return story();
};

/** Install the story's `parameters.tauri` command table before it renders. */
const withTauri: Decorator = (story, context) => {
  setTauriHandlers(context.parameters["tauri"] as TauriHandlers | undefined);
  return story();
};

/**
 * Docs pages render Storybook's own prose and Controls table AROUND the story
 * canvases. `docs.theme` is static, so wrap the container and pick the theme
 * from the Theme global on every render — the docs page re-renders when a
 * global changes.
 */
function MurmurDocsContainer(props: PropsWithChildren<DocsContainerProps>) {
  const store = (props.context as unknown as { store?: { userGlobals?: { globals?: Record<string, unknown> } } })
    .store;
  const theme = store?.userGlobals?.globals?.["theme"];
  return createElement(
    DocsContainer,
    { ...props, theme: isLightTheme(theme) ? themes.light : themes.dark },
    props.children,
  );
}

const preview: Preview = {
  decorators: [withAppearance, withTauri],
  globalTypes: {
    theme: {
      description: "Theme (data-theme)",
      toolbar: {
        title: "Theme",
        icon: "mirror",
        items: [
          { value: "dark", title: "Dark" },
          { value: "light", title: "Light" },
          { value: "system", title: "System" },
        ],
        dynamicTitle: true,
      },
    },
    accent: {
      description: "Accent palette (data-accent)",
      toolbar: {
        title: "Accent",
        icon: "paintbrush",
        items: [
          { value: "purple", title: "Purple (default)" },
          { value: "blue", title: "Blue" },
          { value: "teal", title: "Teal" },
          { value: "green", title: "Green" },
          { value: "orange", title: "Orange" },
          { value: "pink", title: "Pink" },
        ],
        dynamicTitle: true,
      },
    },
    skin: {
      description: "Theme skin (data-skin)",
      toolbar: {
        title: "Skin",
        icon: "component",
        items: [
          { value: "studio", title: "Studio (default)" },
          { value: "paper", title: "Paper" },
        ],
        dynamicTitle: true,
      },
    },
  },
  initialGlobals: {
    theme: "dark",
    accent: "purple",
    skin: "studio",
  },
  parameters: {
    layout: "padded",
    controls: { expanded: true, sort: "requiredFirst" },
    // The canvas paints the app's own ground (body → var(--surface-base)), so
    // Storybook's backgrounds switcher would only fight the theme toolbar.
    backgrounds: { disable: true },
    docs: {
      container: MurmurDocsContainer,
      toc: true,
    },
    options: {
      storySort: {
        order: [
          "Introduction",
          "Design tokens",
          ["Overview", "Colors", "Typography", "Layout", "Glass", "Light theme", "Paper skin", "Accents"],
          "Primitives",
          "Components",
          ["Actions", "Data display", "Feedback", "Forms", "Layout", "Navigation", "Overlays"],
        ],
      },
    },
  },
};

export default preview;
