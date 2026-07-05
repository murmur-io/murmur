// @ts-check
const eslint = require("@eslint/js");
const tseslint = require("typescript-eslint");
const angular = require("angular-eslint");

module.exports = tseslint.config(
  {
    files: ["**/*.ts"],
    extends: [
      eslint.configs.recommended,
      ...tseslint.configs.recommended,
      ...tseslint.configs.stylistic,
      ...angular.configs.tsRecommended,
    ],
    processor: angular.processInlineTemplates,
    rules: {
      "@angular-eslint/directive-selector": [
        "error",
        {
          type: "attribute",
          prefix: "app",
          style: "camelCase",
        },
      ],
      "@angular-eslint/component-selector": [
        "error",
        {
          type: "element",
          // "mur" = the design-system components (<mur-toggle>, <mur-sidebar>…),
          // "app" = feature components.
          prefix: ["app", "mur"],
          style: "kebab-case",
        },
      ],
    },
  },
  {
    files: ["**/*.html"],
    extends: [
      ...angular.configs.templateRecommended,
      ...angular.configs.templateAccessibility,
    ],
    rules: {
      // The design-system form controls (CVA components) count as labelable —
      // the native input they render IS a DOM descendant of the <label>, so
      // the implicit association still works; the rule just can't see through
      // the component boundary.
      "@angular-eslint/template/label-has-associated-control": [
        "error",
        {
          controlComponents: [
            "mur-toggle",
            "mur-input",
            "mur-select",
            "mur-slider",
          ],
        },
      ],
    },
  }
);
