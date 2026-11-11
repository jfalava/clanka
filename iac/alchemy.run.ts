import * as Cloudflare from "@alchemy.run/cloudflare";
import * as Alchemy from "alchemy";
import * as Effect from "effect/Effect";

// The docs are static files and clanka has no API to share the hostname
// with, so the assets Worker takes the custom domain itself.
export const ClankaDocs = Cloudflare.Website.StaticSite("ClankaDocs", {
  name: "clanka-docs",
  cwd: "../docs",
  command: "bun run build",
  outdir: "dist",
  domain: "clanka.jfa.dev",
  workersDev: false,
  compatibility: {
    date: "2026-08-20",
    flags: ["nodejs_compat"],
  },
  assets: { notFoundHandling: "404-page" },
});

export default Alchemy.Stack(
  "Clanka",
  {
    providers: Cloudflare.providers(),
    state: Cloudflare.state(),
  },
  Effect.gen(function* () {
    yield* ClankaDocs;
  }),
);
