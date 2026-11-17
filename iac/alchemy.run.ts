import * as Cloudflare from "@alchemy.run/cloudflare";
import * as Alchemy from "alchemy";
import * as Effect from "effect/Effect";

const observability = {
  enabled: true,
  logs: { enabled: true, invocationLogs: true },
  traces: { enabled: true },
} as const;

// The docs are static files. They have no hostname of their own: the router
// below has clanka.jfa.dev and reaches them over a service binding.
export const ClankaDocs = Cloudflare.Website.StaticSite("ClankaDocs", {
  name: "clanka-docs",
  cwd: "../docs",
  command: "bun run build",
  outdir: "dist",
  // `null`, not left out: Alchemy keeps a hostname attached to a Worker whose
  // `domain` is missing, and the router could then never take it.
  domain: null,
  workersDev: false,
  compatibility: {
    date: "2026-08-20",
    flags: ["nodejs_compat"],
  },
  assets: { notFoundHandling: "404-page" },
});

// Where the diagnostics events of installs that opted in are kept (`clanka
// diagnostics send`): numbers and fixed words, never anything said.
export const Diagnostics = Cloudflare.D1.Database("Diagnostics", {
  name: "clanka-diagnostics",
  primaryLocationHint: "weur",
  migrations: "../ingest/migrations",
});

// The endpoint those installs post to (ingest/). It takes a token per sender,
// let in by a row with its hash (see ingest/migrations/0001_events.sql), and
// deletes events 90 days after they came. Only the router reaches it.
export const Ingest = Cloudflare.Worker("Ingest", {
  name: "clanka-ingest",
  main: "../ingest/src/index.ts",
  workersDev: false,
  observability,
  compatibility: {
    date: "2026-11-10",
  },
  env: { DB: Diagnostics },
  // Daily, for the events past their 90 days.
  crons: ["17 4 * * *"],
});

// clanka.jfa.dev (router/): /ingest is the endpoint, the rest is the docs.
export const Router = Cloudflare.Worker("Router", {
  name: "clanka-router",
  main: "../router/src/index.ts",
  domain: "clanka.jfa.dev",
  workersDev: false,
  observability,
  compatibility: {
    date: "2026-11-10",
  },
  env: {
    INGEST: Ingest,
    DOCS: ClankaDocs,
  },
});

export default Alchemy.Stack(
  "Clanka",
  {
    providers: Cloudflare.providers(),
    state: Cloudflare.state(),
  },
  Effect.gen(function* () {
    const router = yield* Router;
    return { url: router.url };
  }),
);
