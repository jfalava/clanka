import { defineConfig } from "astro/config";
import tailwindcss from "@tailwindcss/vite";
import nimbus, { defineConfig as defineNimbusConfig } from "@cloudflare/nimbus-docs";
import { tableScroll } from "@cloudflare/nimbus-docs/markdown";

const nimbusConfig = defineNimbusConfig({
  site: "https://clanka.jfa.dev",
  title: "CLANKA by JFA",
  description:
    "Long-term memory for chat companions: a soul.md persona plus an encrypted database of what the companion remembers.",
  locale: "en",
  homeLabel: "Clanka",
  github: "https://github.com/jfalava/clanka",
  sidebar: {
    scope: "full",
    // Folder indexes (e.g. cli/index.mdx) become a leading "Overview" leaf;
    // the group header is disclosure-only, not a duplicate nav entry.
    indexDisplay: "overview-leaf",
    overviewLabel: "Overview",
    items: [
      "getting-started",
      {
        label: "Concepts",
        items: ["concepts/souls", "concepts/memory", "concepts/sleep-and-dreams"],
      },
      {
        label: "Guides",
        items: [
          "guides/writing-a-soul",
          "guides/hermes",
          "guides/mcp",
          "guides/repairing-memory",
          "guides/tuning-recall",
        ],
      },
      "configuration",
      {
        label: "CLI",
        items: [
          "cli",
          "cli/init",
          "cli/soul",
          "cli/chat",
          "cli/mcp",
          "cli/hermes",
          "cli/memories",
          "cli/recall",
          "cli/eval",
          "cli/doctor",
          "cli/list",
          "cli/export",
          "cli/import",
          "cli/embedding",
          "cli/tics",
        ],
      },
    ],
  },
  socialImageAlt: "Clanka documentation preview",
});

export default defineConfig({
  base: "/",
  output: "static",
  // Tailwind v4 via its Vite plugin (the integration Astro recommends for
  // Tailwind v4 — replaces the PostCSS plugin, which doesn't build under
  // Astro 7's Vite 8 bundler).
  vite: {
    // Satteri is a build-time Markdown processor. Prefer its host-native
    // binding instead of the browser/WASI condition used by Cloudflare's
    // runtime bundle.
    resolve: { conditions: ["node", "import", "default"] },
    ssr: { external: ["satteri", "satteri-source-parser"] },
    plugins: [tailwindcss()],
  },
  // Hover-prefetch link targets so full-page navigations feel instant without
  // a client-side router.
  prefetch: {
    prefetchAll: true,
    defaultStrategy: "hover",
  },
  integrations: [
    nimbus(nimbusConfig, {
      // Authoring rules are opt-in by design — your repo, your taste. The
      // two below are the load-bearing pair: frontmatter has to validate
      // against the content schema for the page to render properly, and
      // broken internal links are 404s for your readers. Add the others
      // (heading hierarchy, code-block language, style, etc.) when you're
      // ready to enforce them — see `nimbus-docs lint --help`.
      rules: {
        "nimbus/frontmatter-shape": "error",
        "nimbus/internal-link": "error",
      },
      // Wrap wide tables so they scroll instead of overflowing the page
      // (styled by `.nb-table-scroll` in src/styles/prose.css).
      markdown: {
        hastPlugins: [tableScroll()],
      },
    }),
  ],
});
