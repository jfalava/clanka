// The console's reads from the diagnostics database (ingest/migrations/0001_events.sql). Each
// query is one D1 statement run as an Effect, its rows decoded with the Schemas in src/api.ts,
// so a column that changes shape fails here instead of reaching the page.
import { Clock, Data, Effect, Schema } from "effect";

import { EVENTS_PAGE, EventRow, type EventsPage, type Filters, type Overview } from "../api";

const DAY_MS = 24 * 60 * 60 * 1000;

/** D1 did not answer, or answered with rows that are not what the console reads. */
export class StoreError extends Data.TaggedError("StoreError")<{ readonly cause: unknown }> {}

type Value = string | number | null;

const rows = <S extends Schema.Top>(
  db: D1Database,
  schema: S,
  sql: string,
  values: ReadonlyArray<Value>,
) =>
  Effect.tryPromise({
    try: () =>
      db
        .prepare(sql)
        .bind(...values)
        .all(),
    catch: (cause) => new StoreError({ cause }),
  }).pipe(
    Effect.flatMap(({ results }) => Schema.decodeUnknownEffect(Schema.Array(schema))(results)),
    Effect.mapError((cause) => (cause instanceof StoreError ? cause : new StoreError({ cause }))),
  );

/**
 * The WHERE clause the filters make, as ?1 to ?4: since when, which sender, which kind, failures
 * only. Every query binds the four, so one clause serves them all.
 */
const WHERE = `at >= ?1
  AND (?2 IS NULL OR sender = ?2)
  AND (?3 IS NULL OR event = ?3)
  AND (?4 = 0 OR error IS NOT NULL)`;

const bind = (filters: Filters) =>
  Effect.map(Clock.currentTimeMillis, (now): ReadonlyArray<Value> => [
    now - Number(filters.days) * DAY_MS,
    filters.sender || null,
    filters.event || null,
    filters.errors === "only" ? 1 : 0,
  ]);

const Totals = Schema.Struct({
  events: Schema.Finite,
  errors: Schema.Finite,
  installs: Schema.Finite,
  senders: Schema.Finite,
  lastAt: Schema.NullOr(Schema.Finite),
});
const Name = Schema.Struct({ name: Schema.String });

export const overview = (db: D1Database, filters: Filters) =>
  Effect.gen(function* () {
    const values = yield* bind(filters);
    const [totals, byEvent, byError, installs, byDay, senders, events] = yield* Effect.all(
      [
        rows(
          db,
          Totals,
          `SELECT count(*) AS events, count(error) AS errors, count(DISTINCT install) AS installs,
                  count(DISTINCT sender) AS senders, max(at) AS lastAt
           FROM events WHERE ${WHERE}`,
          values,
        ),
        // What an event was about lives in a different field for each kind: a pass's name, the
        // word a plugin reported, the command that ran.
        rows(
          db,
          Schema.Struct({
            event: Schema.String,
            detail: Schema.NullOr(Schema.String),
            count: Schema.Finite,
            errors: Schema.Finite,
            avgMs: Schema.NullOr(Schema.Finite),
          }),
          `SELECT event,
                  coalesce(pass, json_extract(body, '$.what'), json_extract(body, '$.name')) AS detail,
                  count(*) AS count, count(error) AS errors, round(avg(ms)) AS avgMs
           FROM events WHERE ${WHERE}
           GROUP BY event, detail ORDER BY count DESC, event, detail`,
          values,
        ),
        rows(
          db,
          Schema.Struct({
            event: Schema.String,
            error: Schema.String,
            count: Schema.Finite,
            lastAt: Schema.Finite,
          }),
          `SELECT event, error, count(*) AS count, max(at) AS lastAt
           FROM events WHERE ${WHERE} AND error IS NOT NULL
           GROUP BY event, error ORDER BY count DESC, lastAt DESC`,
          values,
        ),
        // SQLite takes the bare columns of an aggregate query from the row that gave max(at):
        // each install shows the host, version and platform it last reported.
        rows(
          db,
          Schema.Struct({
            sender: Schema.String,
            install: Schema.String,
            host: Schema.NullOr(Schema.String),
            version: Schema.NullOr(Schema.String),
            os: Schema.NullOr(Schema.String),
            arch: Schema.NullOr(Schema.String),
            events: Schema.Finite,
            errors: Schema.Finite,
            lastAt: Schema.Finite,
          }),
          `SELECT sender, install, host, version, os, arch, max(at) AS lastAt,
                  count(*) AS events, count(error) AS errors
           FROM events WHERE ${WHERE}
           GROUP BY sender, install ORDER BY lastAt DESC`,
          values,
        ),
        rows(
          db,
          Schema.Struct({ day: Schema.String, count: Schema.Finite, errors: Schema.Finite }),
          `SELECT strftime('%Y-%m-%d', at / 1000, 'unixepoch') AS day,
                  count(*) AS count, count(error) AS errors
           FROM events WHERE ${WHERE}
           GROUP BY day ORDER BY day`,
          values,
        ),
        rows(db, Name, "SELECT DISTINCT sender AS name FROM events ORDER BY name", []),
        rows(db, Name, "SELECT DISTINCT event AS name FROM events ORDER BY name", []),
      ],
      { concurrency: "unbounded" },
    );
    return {
      totals: totals[0] ?? { events: 0, errors: 0, installs: 0, senders: 0, lastAt: null },
      byEvent,
      byError,
      installs,
      byDay,
      senders: senders.map(({ name }) => name),
      events: events.map(({ name }) => name),
    } satisfies Overview;
  });

/**
 * The events that match, newest first by when they happened, `EVENTS_PAGE` at a time, from just
 * before the cursor `before` (`<at>-<id>`) when given. Sent in batches, events arrive out of the
 * order they happened in, so ids alone would not page them by time.
 */
export const events = (db: D1Database, filters: Filters, before: string | undefined) =>
  Effect.gen(function* () {
    const values = yield* bind(filters);
    const [beforeAt, beforeId] = before?.split("-").map(Number) ?? [null, null];
    const page = yield* rows(
      db,
      EventRow,
      `SELECT id, received_at AS receivedAt, sender, install, at, event, host, version, os, arch,
              companion, pass, error, ms, body
       FROM events WHERE ${WHERE} AND (?5 IS NULL OR at < ?5 OR (at = ?5 AND id < ?6))
       ORDER BY at DESC, id DESC LIMIT ?7`,
      [...values, beforeAt ?? null, beforeId ?? null, EVENTS_PAGE + 1],
    );
    const shown = page.slice(0, EVENTS_PAGE);
    const last = shown.at(-1);
    return {
      events: shown,
      next: page.length > EVENTS_PAGE && last ? `${last.at}-${last.id}` : null,
    } satisfies EventsPage;
  });
