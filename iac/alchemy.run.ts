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

// The telemetry console (admin/): a Foldkit page that reads the diagnostics
// database and changes nothing in it. The router mounts it at /admin, which a
// Cloudflare Access application guards; the Worker checks each request's Access
// token against that application's audience too, and refuses everything while
// ACCESS_AUD is empty.
export const ClankaAdmin = Cloudflare.Website.Foldkit("ClankaAdmin", {
  name: "clanka-admin",
  rootDir: "../admin",
  main: "src/worker.ts",
  // A service binding skips the asset layer anyway: every request reaches the
  // Worker, which hands the page's files to ASSETS itself.
  assets: { notFoundHandling: "single-page-application", runWorkerFirst: true },
  domain: null,
  workersDev: false,
  observability,
  compatibility: {
    date: "2026-11-10",
  },
  env: {
    DB: Diagnostics,
    ACCESS_TEAM_DOMAIN: "jfa-d.cloudflareaccess.com",
    // The audience tag of the Access application `clanka` on clanka.jfa.dev/admin.
    ACCESS_AUD: "25ecf4f67717d8d68e300213f2b8a9604e709f3195c569da6c1f9ac879b73acb",
  },
});

// clanka.jfa.dev (router/): /ingest is the endpoint, /admin the console, the
// rest is the docs.
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
    ADMIN: ClankaAdmin,
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
