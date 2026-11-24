// What the page asks `clanka web` for. The server is Rust (crates/cli/src/web/mod.rs) and answers
// with the core's own types as JSON, so the names here are the Rust fields', snake case and all.
// test/api.test.ts decodes what the server really sends with these Schemas: a field renamed on one
// side fails there or in the server's own tests. Every request is a GET; the page changes nothing.
import { Effect, Schema } from "effect";

const Time = Schema.Finite;
const Id = Schema.Finite;

export const KINDS = [
  "user_fact",
  "preference",
  "episode",
  "relationship",
  "promise",
  "opinion",
  "open_thread",
  "persona_note",
] as const;
export const LEVELS = ["faint", "normal", "vivid", "seared"] as const;

export const Kind = Schema.Literals(KINDS);
export type Kind = typeof Kind.Type;
export const Level = Schema.Literals(LEVELS);
export type Level = typeof Level.Type;

/** A soul.md on this machine. One that does not load has an `error` and no `id`. */
export const Listed = Schema.Struct({
  soul: Schema.String,
  id: Schema.NullOr(Schema.String),
  name: Schema.NullOr(Schema.String),
  /** The size of its database; null when it was never opened. */
  db_bytes: Schema.NullOr(Schema.Finite),
  error: Schema.NullOr(Schema.String),
});
export type Listed = typeof Listed.Type;

export const Mood = Schema.Struct({
  valence: Schema.Finite,
  energy: Schema.Finite,
  warmth: Schema.Finite,
  cause: Schema.String,
});

export const SectionVersion = Schema.Struct({
  id: Id,
  content: Schema.String,
  persona_hash: Schema.String,
  created_at: Time,
});

export const Overview = Schema.Struct({
  id: Schema.String,
  name: Schema.String,
  soul: Schema.String,
  frozen: Schema.Boolean,
  archived: Schema.Boolean,
  persona_changed: Schema.Boolean,
  session: Schema.Literals(["dream", "conversation", "none"]),
  nights_waiting: Schema.Finite,
  mood: Schema.NullOr(Mood),
  note: Schema.NullOr(Schema.String),
  /** Each section's versions, oldest first; the last is current. */
  sections: Schema.Array(
    Schema.Struct({
      name: Schema.Literals(["about_user", "relationship"]),
      versions: Schema.Array(SectionVersion),
    }),
  ),
});
export type Overview = typeof Overview.Type;

export const Memory = Schema.Struct({
  id: Id,
  kind: Kind,
  level: Level,
  facts: Schema.Record(Schema.String, Schema.Unknown),
  interpretation: Schema.String,
  status: Schema.Literals(["active", "superseded"]),
  superseded_by: Schema.NullOr(Id),
  occurred_at: Schema.NullOr(Time),
  persona_hash: Schema.String,
  strength: Schema.NullOr(Schema.Finite),
  urgency: Schema.NullOr(Schema.Literals(["simmering", "hot"])),
  resolved_at: Schema.NullOr(Time),
  valence: Schema.NullOr(Schema.Finite),
  dreamt: Schema.Boolean,
  recall_count: Schema.Finite,
  last_recalled_session: Schema.NullOr(Id),
  source_turns: Schema.Array(Id),
  created_at: Time,
  updated_at: Time,
});
export type Memory = typeof Memory.Type;

/** A fact its memory's turns never said; `fact` is null when the memory cites no turns at all. */
export const FactIssue = Schema.Struct({
  memory: Id,
  fact: Schema.NullOr(Schema.Tuple([Schema.String, Schema.String])),
});
export type FactIssue = typeof FactIssue.Type;

export const Detail = Schema.Struct({
  versions: Schema.Array(
    Schema.Struct({
      id: Id,
      interpretation: Schema.String,
      level: Level,
      persona_hash: Schema.String,
      reason: Schema.String,
      drift_magnitude: Schema.NullOr(Schema.Finite),
      created_at: Time,
    }),
  ),
  turns: Schema.Array(
    Schema.Struct({
      id: Id,
      session: Id,
      speaker: Schema.Literals(["user", "companion"]),
      content: Schema.String,
      echo: Schema.Boolean,
      created_at: Time,
    }),
  ),
});
export type Detail = typeof Detail.Type;

export const Found = Schema.Struct({
  found: Schema.Array(
    Schema.Struct({ memory: Memory, relevance: Schema.Finite, score: Schema.Finite }),
  ),
  limit: Schema.Finite,
  min_relevance: Schema.Finite,
  min_score: Schema.Finite,
});
export type Found = typeof Found.Type;

export const WriteEntry = Schema.Struct({
  id: Id,
  actor: Schema.String,
  action: Schema.String,
  target: Schema.String,
  target_id: Schema.NullOr(Id),
  payload: Schema.Unknown,
  created_at: Time,
});
export type WriteEntry = typeof WriteEntry.Type;

const Refusal = Schema.Struct({ error: Schema.String });
const refusal = Schema.decodeUnknownOption(Refusal);

/**
 * A GET of the API with this run's token, decoded. Fails with a sentence to show: the server's own
 * `error`, or what went wrong reaching it.
 */
export const get = <A>(
  token: string,
  path: string,
  schema: Schema.Codec<A, unknown>,
): Effect.Effect<A, string> =>
  Effect.tryPromise({
    try: async () => {
      const response = await fetch(path, { headers: { Authorization: `Bearer ${token}` } });
      const body: unknown = await response.json().catch(() => null);
      if (!response.ok) {
        const said = refusal(body);
        throw new Error(
          said._tag === "Some" ? said.value.error : `clanka web answered ${response.status}.`,
        );
      }
      return body;
    },
    catch: (cause) =>
      cause instanceof TypeError
        ? "clanka web did not answer. Is it still running?"
        : cause instanceof Error
          ? cause.message
          : String(cause),
  }).pipe(
    Effect.flatMap((body) =>
      Schema.decodeUnknownEffect(schema)(body).pipe(
        Effect.mapError(
          () => `clanka web sent something this page cannot read from ${path.split("?")[0]}.`,
        ),
      ),
    ),
  );

const companion = (id: string) => `/api/companions/${id}`;

export const paths = {
  companions: "/api/companions",
  overview: companion,
  memories: (id: string) => `${companion(id)}/memories`,
  facts: (id: string) => `${companion(id)}/facts`,
  memory: (id: string, memory: number) => `${companion(id)}/memories/${memory}`,
  recall: (id: string, query: string) => `${companion(id)}/recall?q=${encodeURIComponent(query)}`,
  log: (id: string, since: number | null) =>
    since === null ? `${companion(id)}/log` : `${companion(id)}/log?since=${since}`,
};
