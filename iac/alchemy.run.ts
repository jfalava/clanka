import * as Cloudflare from "@alchemy.run/cloudflare";
import * as Alchemy from "alchemy";
import * as Effect from "effect/Effect";

import { loadDeployConfig } from "./src/deploy-config";

// Which hostname, and whether the docs and the console are part of it: see
// src/deploy-config.ts and README.md. Without a deploy config, the stack is the
// endpoint alone.
const config = loadDeployConfig();

const observability = {
  enabled: true,
  logs: { enabled: true, invocationLogs: true },
  traces: { enabled: true },
} as const;

// The docs are static files. They have no hostname of their own: the router
// below has the deployment's and reaches them over a service binding.
const ClankaDocs = Cloudflare.Website.StaticSite("ClankaDocs", {
  name: `${config.name}-docs`,
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
  name: `${config.name}-diagnostics`,
  primaryLocationHint: "weur",
  migrations: "../ingest/migrations",
});

// The endpoint those installs post to (ingest/). It takes a token per sender,
// let in by a row with its hash (see ingest/migrations/0001_events.sql), and
// deletes events 90 days after they came. Only the router reaches it.
export const Ingest = Cloudflare.Worker("Ingest", {
  name: `${config.name}-ingest`,
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
// token against that application's audience too. Deployed only with one.
const ClankaAdmin = (access: NonNullable<typeof config.access>) =>
  Cloudflare.Website.Foldkit("ClankaAdmin", {
    name: `${config.name}-admin`,
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
      ACCESS_TEAM_DOMAIN: access.teamDomain,
      ACCESS_AUD: access.aud,
    },
  });

// The deployment's hostname (router/): /ingest is the endpoint, /admin the
// console, the rest is the docs; a part left out answers 404.
export const Router = Cloudflare.Worker("Router", {
  name: `${config.name}-router`,
  main: "../router/src/index.ts",
  // `null` with no domain, so none stays attached (see ClankaDocs).
  domain: config.domain ?? null,
  workersDev: config.domain === undefined,
  observability,
  compatibility: {
    date: "2026-11-10",
  },
  env: {
    INGEST: Ingest,
    ...(config.access ? { ADMIN: ClankaAdmin(config.access) } : {}),
    ...(config.docs ? { DOCS: ClankaDocs } : {}),
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
