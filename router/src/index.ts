// The Worker on clanka.jfa.dev: /ingest goes to the diagnostics endpoint
// (ingest/) and everything else to the docs. Both are reached over service
// bindings, never their hostnames: a subrequest to the same zone answers 522.
import { Hono, type Handler } from "hono";
import type { Fetcher } from "@cloudflare/workers-types";

export interface Env {
  INGEST: Fetcher;
  DOCS: Fetcher;
}

type App = { Bindings: Env };

const INGEST_PREFIX = "/ingest";

/** The endpoint knows its own paths only: `/ingest/v1/events` reaches it as `/v1/events`. */
const forwardIngest: Handler<App> = async (c) => {
  const url = new URL(c.req.raw.url);
  url.pathname = url.pathname.slice(INGEST_PREFIX.length) || "/";

  return c.env.INGEST.fetch(new Request(url, c.req.raw));
};

// No list of the docs' paths is kept here, unlike the routers in front of a
// docs Worker that runs code: these docs are static files, which cost nothing
// to miss, and a list would 404 the first section someone forgot to add to it.
const forwardDocs: Handler<App> = async (c) => c.env.DOCS.fetch(c.req.raw);

export default new Hono<App>()
  .all(INGEST_PREFIX, forwardIngest)
  .all(`${INGEST_PREFIX}/*`, forwardIngest)
  .all("*", forwardDocs);
