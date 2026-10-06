import vue from "eslint-plugin-vue";
import globals from "globals";
import tseslint from "typescript-eslint";

const generatedAndRuntime = [
  "packages/contracts/src/generated.ts",
  "services/agent-manager/src/environment-defaults.json",
  "specs/schemas/*.schema.json",
  "services/**/native/**",
  "packages/host-runtime/native/**",
  "**/vendor/**",
  "node_modules/**",
  "dist/**",
  "runtime/**",
  "services/*/runtime/**",
  ".local/**",
  "coverage/**",
];

export default [
  { ignores: generatedAndRuntime },
  {
    files: ["**/*.{js,mjs,cjs}"],
    languageOptions: { globals: { ...globals.node, ...globals.browser } },
  },
  {
    files: ["**/*.{ts,mts,cts,vue}"],
    languageOptions: {
      parser: tseslint.parser,
      globals: { ...globals.node, ...globals.browser },
    },
  },
  ...vue.configs["flat/essential"].map((config) => ({
    ...config,
    files: ["**/*.vue"],
  })),
  {
    files: ["**/*.vue"],
    languageOptions: {
      parserOptions: {
        parser: tseslint.parser,
        extraFileExtensions: [".vue"],
        sourceType: "module",
      },
      globals: globals.browser,
    },
    rules: { "vue/multi-word-component-names": "off" },
  },
  {
    files: ["**/*.{js,mjs,cjs,ts,mts,cts,vue}"],
    rules: {
      "constructor-super": "error",
      "for-direction": "error",
      "getter-return": "error",
      "no-constant-binary-expression": "error",
      "no-dupe-class-members": "error",
      "no-dupe-else-if": "error",
      "no-duplicate-case": "error",
      "no-self-assign": "error",
      "no-unreachable": "error",
      "no-unreachable-loop": "error",
      "no-unsafe-negation": "error",
      "use-isnan": "error",
    },
  },
];
