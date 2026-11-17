// The endpoint `clanka diagnostics send` posts to: it checks the sender's
// token, refuses anything that is not a diagnostics event, and keeps the rest
// in D1 for RETENTION_DAYS. The router (router/) mounts it at
// clanka.jfa.dev/ingest; it has no hostname of its own.
//
// A sender makes its own token (`clanka diagnostics token --new`) and gives
// its SHA-256 hash to whoever runs this, who adds it to the `tokens` table.
// The token itself is never stored here, or seen by anyone but its sender.
//
// Anyone may run their own: the request is plain enough for any store. A POST
// to /v1/events with `Authorization: Bearer <token>` and one JSON object per
// line (NDJSON), at most MAX_EVENTS lines and MAX_BODY bytes.
import { Clock, Data, Effect } from "effect";

export interface Env {
  DB: D1Database;
}

const MAX_BODY = 512 * 1024;
const MAX_EVENTS = 500;
const RETENTION_DAYS = 90;
const DAY_MS = 24 * 60 * 60 * 1000;

// Every string in an event is a word clanka spells out itself: a command, an
// error's kind, a version, the end of an ID. A longer one, or one with other
// characters, is not an event, whatever sent it. This keeps a changed or
// broken client from storing text here. It cannot tell a short phrase from a
// word, so it limits what gets in and proves nothing about it.
const WORD = /^[A-Za-z0-9_.\/ -]{0,64}$/;
const NAME = /^[a-z_]{1,32}$/;
const INSTALL = /^[0-9a-f]{32}$/;
const MAX_ITEMS = 32;
const MAX_DEPTH = 3;

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

function isValue(value: unknown, depth: number): value is Json {
  if (value === null || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (typeof value === "string") return WORD.test(value);
  if (typeof value !== "object" || depth >= MAX_DEPTH) return false;
  if (Array.isArray(value)) {
    return value.length <= MAX_ITEMS && value.every((item) => isValue(item, depth + 1));
  }
  const fields = Object.entries(value);
  return (
    fields.length <= MAX_ITEMS &&
    fields.every(([name, field]) => NAME.test(name) && isValue(field, depth + 1))
  );
}

type Event = { [key: string]: Json } & { event: string; at: number; install: string };

function isEvent(value: unknown): value is Event {
  if (!isValue(value, 0) || value === null || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }
  const { event, at, install } = value;
  return (
    typeof event === "string" &&
    NAME.test(event) &&
    typeof at === "number" &&
    Number.isSafeInteger(at) &&
    at > 0 &&
    typeof install === "string" &&
    INSTALL.test(install)
  );
}

/** A line as JSON, or `undefined` for one that is not. */
function parse(line: string): unknown {
  try {
    return JSON.parse(line);
  } catch {
    return undefined;
  }
}

const word = (value: Json | undefined) => (typeof value === "string" ? value : null);

/** An event as it is kept: the fields asked about most, and all of it in `body`. */
interface Row {
  install: string;
  digest: string;
  at: number;
  event: string;
  host: string | null;
  version: string | null;
  os: string | null;
  arch: string | null;
  companion: string | null;
  pass: string | null;
  error: string | null;
  ms: number | null;
  body: string;
}

const json = (status: number, body: unknown) => Response.json(body, { status });

/** A request turned away: the status and what its sender is told. */
class Refusal extends Data.TaggedError("Refusal")<{
  readonly status: number;
  readonly error: string;
}> {}

/** The database did not answer, or not with what was asked. */
class StoreError extends Data.TaggedError("StoreError")<{ readonly cause: unknown }> {}

const refuse = (status: number, error: string) => new Refusal({ status, error });

const store = <A>(query: () => Promise<A>) =>
  Effect.tryPromise({ try: query, catch: (cause) => new StoreError({ cause }) });

const sha256 = (text: string) =>
  Effect.promise(async () => {
    const hash = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
    return Array.from(new Uint8Array(hash), (byte) => byte.toString(16).padStart(2, "0")).join("");
  });

/**
 * The request's body as text. Read piece by piece, so one over `limit` bytes
 * is refused when it gets there, whatever its Content-Length said, and never
 * held whole.
 */
const body = (request: Request, limit: number) =>
  Effect.tryPromise({
    try: async () => {
      const reader = request.body?.getReader();
      if (!reader) return "";
      const decoder = new TextDecoder();
      let text = "";
      let length = 0;
      for (;;) {
        const { done, value } = await reader.read();
        if (done) return text + decoder.decode();
        length += value.byteLength;
        if (length > limit) {
          await reader.cancel();
          return null;
        }
        text += decoder.decode(value, { stream: true });
      }
    },
    catch: () => refuse(400, "the request's body did not arrive whole"),
  });

const receive = (request: Request, env: Env) =>
  Effect.gen(function* () {
    const token = /^Bearer (\S+)$/.exec(request.headers.get("Authorization") ?? "")?.[1];
    if (!token) return yield* refuse(401, "send the token as Authorization: Bearer <token>");
    // The token is looked up by its hash, so it is never compared as text.
    const hash = yield* sha256(token);
    const sender = yield* store(() =>
      env.DB.prepare("SELECT label FROM tokens WHERE hash = ?1 AND revoked_at IS NULL")
        .bind(hash)
        .first<string>("label"),
    );
    if (sender === null) return yield* refuse(401, "this token is not known here, or was revoked");

    const text = yield* body(request, MAX_BODY);
    if (text === null) return yield* refuse(413, `a request holds at most ${MAX_BODY} bytes`);
    const lines = text.split("\n").filter((line) => line.trim() !== "");
    if (lines.length > MAX_EVENTS) {
      return yield* refuse(413, `a request holds at most ${MAX_EVENTS} events`);
    }

    // A line that is not an event is dropped and counted, and the rest are
    // kept: a client sends its log in order, and one refused request would
    // hold back everything after it for good.
    const rows: Row[] = [];
    for (const line of lines) {
      const event = parse(line);
      if (!isEvent(event)) continue;
      rows.push({
        install: event.install,
        digest: (yield* sha256(line)).slice(0, 32),
        at: event.at,
        event: event.event,
        host: word(event.host),
        version: word(event.version),
        os: word(event.os),
        arch: word(event.arch),
        companion: word(event.companion),
        pass: word(event.pass),
        error: word(event.error),
        ms: typeof event.ms === "number" && Number.isSafeInteger(event.ms) ? event.ms : null,
        body: JSON.stringify(event),
      });
    }
    const refused = lines.length - rows.length;
    if (rows.length === 0) return json(200, { stored: 0, duplicates: 0, refused });

    // One statement for the whole request: D1 answers each statement over the
    // network, and a statement takes at most 100 bound values.
    const now = yield* Clock.currentTimeMillis;
    const written = yield* store(() =>
      env.DB.prepare(
        `INSERT OR IGNORE INTO events (received_at, sender, install, digest, at, event, host,
           version, os, arch, companion, pass, error, ms, body)
         SELECT ?1, ?2, json_extract(value, '$.install'), json_extract(value, '$.digest'),
           json_extract(value, '$.at'), json_extract(value, '$.event'),
           json_extract(value, '$.host'), json_extract(value, '$.version'),
           json_extract(value, '$.os'), json_extract(value, '$.arch'),
           json_extract(value, '$.companion'), json_extract(value, '$.pass'),
           json_extract(value, '$.error'), json_extract(value, '$.ms'),
           json_extract(value, '$.body')
         FROM json_each(?3)`,
      )
        .bind(now, sender, JSON.stringify(rows))
        .run(),
    );
    const stored = written.meta.changes;
    return json(200, { stored, duplicates: rows.length - stored, refused });
  });

const route = (request: Request, env: Env) =>
  Effect.gen(function* () {
    if (new URL(request.url).pathname !== "/v1/events") {
      return yield* refuse(
        404,
        "POST diagnostics events to https://clanka.jfa.dev/ingest/v1/events; see https://clanka.jfa.dev/cli/diagnostics/",
      );
    }
    if (request.method !== "POST") return yield* refuse(405, "POST only");
    return yield* receive(request, env);
  });

// Rows are written in the order they arrive, so the oldest have the lowest
// IDs: each pass looks at the first rows only, however many are kept.
const EXPIRE_ROWS = 10_000;
const EXPIRE_PASSES = 20;

const expire = (env: Env) =>
  Effect.gen(function* () {
    const before = (yield* Clock.currentTimeMillis) - RETENTION_DAYS * DAY_MS;
    for (let pass = 0; pass < EXPIRE_PASSES; pass++) {
      const gone = yield* store(() =>
        env.DB.prepare(
          `DELETE FROM events WHERE id IN (
             SELECT id FROM (SELECT id, received_at FROM events ORDER BY id LIMIT ?2)
             WHERE received_at < ?1)`,
        )
          .bind(before, EXPIRE_ROWS)
          .run(),
      );
      if (gone.meta.changes < EXPIRE_ROWS) return;
    }
  });

export default {
  fetch: (request, env) =>
    Effect.runPromise(
      route(request, env).pipe(
        Effect.catchTags({
          Refusal: ({ status, error }) => Effect.succeed(json(status, { error })),
          // Not a 2xx, so the sender keeps its events and sends them again.
          StoreError: ({ cause }) =>
            Effect.sync(() => {
              console.error(JSON.stringify({ message: "the database failed", cause: String(cause) }));
              return json(503, { error: "the events could not be stored; send them again later" });
            }),
        }),
      ),
    ),
  // A night that fails is a failed invocation in the Worker's logs.
  scheduled: (_controller, env) => Effect.runPromise(expire(env)),
} satisfies ExportedHandler<Env>;
