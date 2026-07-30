// @ts-check
import nextConfig from "eslint-config-next";
import eslintConfigPrettier from "eslint-config-prettier";

/** @type {import("eslint").Linter.Config[]} */
const config = [
  {
    ignores: [
      ".next/**",
      "node_modules/**",
      "playwright-report/**",
      "test-results/**",
      "next-env.d.ts",
    ],
  },
  ...nextConfig,
  // Turn off stylistic rules that Prettier owns, so the two tools never
  // fight each other. Must stay last.
  eslintConfigPrettier,
];

export default config;
