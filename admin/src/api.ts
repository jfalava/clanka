// What the console asks the Worker for. The Worker (src/worker.ts) serves it with HttpApiBuilder
// and the page (src/main.ts) calls it through HttpApiClient, so both read the same Schemas.
// Every endpoint reads: the console changes nothing in the diagnostics database.
import { Schema } from "effect";
import { HttpApi, HttpApiEndpoint, HttpApiGroup, HttpApiSchema } from "effect/http-api";

export const Range = Schema.Literals(["1", "7", "30", "90"]);
export type Range = typeof Range.Type;

/** Which events to look at: the last `days`, from one sender, of one kind, only failures. */
export const Filters = Schema.Struct({
  days: Range,
  sender: Schema.optional(Schema.String),
  event: Schema.optional(Schema.String),
  errors: Schema.optional(Schema.Literal("only")),
});
export type Filters = typeof Filters.Type;

const Count = Schema.Finite;
const Time = Schema.Finite;
const Word = Schema.NullOr(Schema.String);

export const Overview = Schema.Struct({
  totals: Schema.Struct({
    events: Count,
    errors: Count,
    installs: Count,
    senders: Count,
    lastAt: Schema.NullOr(Time),
  }),
  /** Each kind of event, with the pass, plugin report or command it was about. */
  byEvent: Schema.Array(
    Schema.Struct({
      event: Schema.String,
      detail: Word,
      count: Count,
      errors: Count,
      avgMs: Schema.NullOr(Schema.Finite),
    }),
  ),
  byError: Schema.Array(
    Schema.Struct({ event: Schema.String, error: Schema.String, count: Count, lastAt: Time }),
  ),
  installs: Schema.Array(
    Schema.Struct({
      sender: Schema.String,
      install: Schema.String,
      host: Word,
      version: Word,
      os: Word,
      arch: Word,
      events: Count,
      errors: Count,
      lastAt: Time,
    }),
  ),
  byDay: Schema.Array(Schema.Struct({ day: Schema.String, count: Count, errors: Count })),
  /** Every sender and kind kept, whatever the filters, for the filter menus. */
  senders: Schema.Array(Schema.String),
  events: Schema.Array(Schema.String),
});
export type Overview = typeof Overview.Type;

export const EventRow = Schema.Struct({
  id: Schema.Finite,
  receivedAt: Time,
  sender: Schema.String,
  install: Schema.String,
  at: Time,
  event: Schema.String,
  host: Word,
  version: Word,
  os: Word,
  arch: Word,
  companion: Word,
  pass: Word,
  error: Word,
  ms: Schema.NullOr(Schema.Finite),
  /** The whole event as it was sent, as JSON text. */
  body: Schema.String,
});
export type EventRow = typeof EventRow.Type;

export const EventsPage = Schema.Struct({
  events: Schema.Array(EventRow),
  /** Pass as `before` for the page after this one (`<at>-<id>` of its last event); null on the
   *  last page. */
  next: Schema.NullOr(Schema.String),
});
export type EventsPage = typeof EventsPage.Type;

export const EVENTS_PAGE = 50;

/** Where a page of events starts: just older than the event with this `at` and id. */
export const Cursor = Schema.String.check(Schema.isPattern(/^\d+-\d+$/));

const failure = <const Tag extends string>(tag: Tag, status: number) =>
  Schema.TaggedStruct(tag, { error: Schema.String }).pipe(HttpApiSchema.status(status));

export const Forbidden = failure("Forbidden", 403);
export const Unavailable = failure("Unavailable", 503);
const error = [Forbidden, Unavailable] as const;

export class TelemetryApi extends HttpApiGroup.make("telemetry").add(
  HttpApiEndpoint.get("overview", "/admin/api/overview", {
    query: Filters,
    success: Overview,
    error,
  }),
  HttpApiEndpoint.get("events", "/admin/api/events", {
    query: Schema.Struct({ ...Filters.fields, before: Schema.optional(Cursor) }),
    success: EventsPage,
    error,
  }),
) {}

export class AdminApi extends HttpApi.make("admin").add(TelemetryApi) {}

/** The server's message for a declared failure, or a hint for a request that never got one. */
export const errorText = (cause: unknown) =>
  typeof cause === "object" && cause !== null && "error" in cause && typeof cause.error === "string"
    ? cause.error
    : "The request failed. Check your connection and Access session, then retry.";
