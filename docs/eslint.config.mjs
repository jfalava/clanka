import eslintPluginAstro from "eslint-plugin-astro";

// No type assertions or non-null assertions in .astro files.
const strictSyntax = [
  {
    selector: "TSAsExpression:not([typeAnnotation.typeName.name='const'])",
    message: "No type assertions; use an annotation, satisfies, or a guard.",
  },
  { selector: "TSTypeAssertion", message: "No type assertions; use an annotation, satisfies, or a guard." },
  { selector: "TSNonNullExpression", message: "No non-null assertions; narrow the value instead." },
];

export default [
  ...eslintPluginAstro.configs.recommended,
  {
    files: ["**/*.astro", "**/*.astro/*.ts", "*.astro/*.ts"],
    rules: { "no-restricted-syntax": ["error", ...strictSyntax] },
  },
];
