// clanka-admin: the telemetry console at clanka.jfa.dev/admin. The router (router/) sends it
// every /admin request over a service binding, which skips the platform's asset routing, so this
// Worker serves the page's files from ASSETS itself and answers /admin/api with the reads in
// src/api.ts, after checking the request came through Cloudflare Access.
import { Context, Effect, Layer } from "effect";
import { HttpRouter, HttpServer } from "effect/http";
import { HttpApiBuilder } from "effect/http-api";

import { AdminApi, Forbidden, Unavailable } from "./api";
import { verifyAccess } from "./server/access";
import * as Telemetry from "./server/telemetry";

export interface Env {
  DB: D1Database;
  ASSETS: Fetcher;
  /** The Access team's domain, `<team>.cloudflareaccess.com`. */
  ACCESS_TEAM_DOMAIN: string;
  /** The Access application's audience tag. Empty until the application exists. */
  ACCESS_AUD: string;
}

class Bindings extends Context.Service<Bindings, Env>()("clanka/admin/Bindings") {}

const unavailable = (error: string) => Unavailable.make({ error });

/** A read that fails says the database is out of reach; what went wrong goes to the logs. */
const read = <A>(query: (db: D1Database) => Effect.Effect<A, Telemetry.StoreError>) =>
  Bindings.use((env) => query(env.DB)).pipe(
    Effect.tapError((error) => Effect.logError("Telemetry read failed", error.cause)),
    Effect.mapError(() => unavailable("The diagnostics database did not answer. Retry in a moment.")),
  );

const TelemetryHandlers = HttpApiBuilder.group(AdminApi, "telemetry", (handlers) =>
  handlers
    .handle("overview", ({ query }) => read((db) => Telemetry.overview(db, query)))
    .handle("events", ({ query: { before, ...filters } }) =>
      read((db) => Telemetry.events(db, filters, before)),
    ),
);

const api = HttpRouter.toWebHandler(
  HttpApiBuilder.layer(AdminApi).pipe(
    Layer.provide(TelemetryHandlers),
    Layer.provide(HttpServer.layerServices),
  ),
  { disableLogger: true },
);

const json = (status: number, body: unknown) => Response.json(body, { status });

const TEAM_DOMAIN = /^[a-z0-9-]+\.cloudflareaccess\.com$/;

/** The refusal for a request Access did not let through, or none. */
const refusal = (request: Request, env: Env) =>
  Effect.gen(function* () {
    if (!TEAM_DOMAIN.test(env.ACCESS_TEAM_DOMAIN) || !env.ACCESS_AUD) {
      return json(503, unavailable("Cloudflare Access is not set up for the console."));
    }
    const assertion = request.headers.get("Cf-Access-Jwt-Assertion");
    const isAllowed =
      assertion !== null &&
      (yield* verifyAccess(assertion, env.ACCESS_TEAM_DOMAIN, env.ACCESS_AUD).pipe(
        Effect.tapError((cause) => Effect.logError("Access keys could not be read", cause)),
        Effect.orElseSucceed(() => null),
      ));
    if (isAllowed === null) {
      return json(503, unavailable("Cloudflare Access could not be reached. Retry in a moment."));
    }
    return isAllowed ? null : json(403, Forbidden.make({ error: "Sign in through Cloudflare Access." }));
  });

const answer = async (request: Request, env: Env): Promise<Response> => {
  if (request.method !== "GET" && request.method !== "HEAD") {
    return json(405, { _tag: "MethodNotAllowed", error: "The console only reads." });
  }
  const refused = await Effect.runPromise(refusal(request, env));
  if (refused) return refused;
  const response = await api.handler(request, Context.make(Bindings, env));
  // Effect answers a request it can't route or decode with an empty body; keep every one JSON.
  if (response.body === null && response.status >= 400) {
    return json(response.status, { _tag: "BadRequest", error: "There is no such request." });
  }
  return response;
};

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(request.url);
    if (pathname !== "/admin/api" && !pathname.startsWith("/admin/api/")) {
      return env.ASSETS.fetch(request);
    }
    const response = await answer(request, env).catch((cause: unknown) => {
      console.error("Admin request failed", cause);
      return json(500, { _tag: "ServerError", error: "The console could not answer." });
    });
    const headers = new Headers(response.headers);
    headers.set("Cache-Control", "no-store");
    headers.set("X-Content-Type-Options", "nosniff");
    return new Response(request.method === "HEAD" ? null : response.body, {
      status: response.status,
      headers,
    });
  },
} satisfies ExportedHandler<Env>;
