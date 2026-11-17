// The Worker on the deployment's hostname (clanka.jfa.dev for this
// repository's): /ingest goes to the diagnostics endpoint (ingest/), /admin to
// the telemetry console (admin/) and everything else to the docs. All three are
// reached over service bindings, never their hostnames: a subrequest to the
// same zone answers 522. A deployment may leave out the console and the docs
// (iac/src/deploy-config.ts); their paths then answer 404.
import { Hono, type Handler } from "hono";
import type { Fetcher } from "@cloudflare/workers-types";

export interface Env {
  INGEST: Fetcher;
  ADMIN?: Fetcher;
  DOCS?: Fetcher;
}

type App = { Bindings: Env };

const INGEST_PREFIX = "/ingest";

/** The endpoint knows its own paths only: `/ingest/v1/events` reaches it as `/v1/events`. */
const forwardIngest: Handler<App> = async (c) => {
  const url = new URL(c.req.raw.url);
  url.pathname = url.pathname.slice(INGEST_PREFIX.length) || "/";

  return c.env.INGEST.fetch(new Request(url, c.req.raw));
};

const ADMIN_PREFIX = "/admin";

/**
 * The console keeps its prefix: its files are built under /admin/ and its API
 * is /admin/api. Cloudflare Access guards /admin before the request gets here.
 */
const forwardAdmin: Handler<App> = async (c) =>
  c.env.ADMIN ? c.env.ADMIN.fetch(c.req.raw) : c.notFound();

// No list of the docs' paths is kept here, unlike the routers in front of a
// docs Worker that runs code: these docs are static files, which cost nothing
// to miss, and a list would 404 the first section someone forgot to add to it.
const forwardDocs: Handler<App> = async (c) =>
  c.env.DOCS ? c.env.DOCS.fetch(c.req.raw) : c.notFound();

export default new Hono<App>()
  .all(INGEST_PREFIX, forwardIngest)
  .all(`${INGEST_PREFIX}/*`, forwardIngest)
  .all(ADMIN_PREFIX, forwardAdmin)
  .all(`${ADMIN_PREFIX}/*`, forwardAdmin)
  .all("*", forwardDocs);
