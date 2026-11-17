import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import worker, { type Env } from "../src/index";

const DAY_MS = 24 * 60 * 60 * 1000;
const INSTALL = "5f0c".repeat(8);

/** SQLite with the real schema, behind the part of D1 the Worker uses. */
function endpoint() {
  const db = new Database(":memory:");
  db.exec(readFileSync(`${import.meta.dir}/../migrations/0001_events.sql`, "utf8"));
  const statement = (sql: string, values: (string | number | null)[]) => ({
    bind: (...bound: (string | number | null)[]) => statement(sql, bound),
    first: async (column: string) =>
      db.query<Record<string, unknown>, (string | number | null)[]>(sql).get(...values)?.[column] ??
      null,
    run: async () => ({ meta: { changes: db.query(sql).run(...values).changes } }),
  });
  // A test double: only `prepare`, `bind`, `first` and `run` are there.
  const env = { DB: { prepare: (sql: string) => statement(sql, []) } } as unknown as Env;

  const allow = (token: string, label: string, revokedAt: number | null = null) =>
    db
      .query("INSERT INTO tokens (hash, label, created_at, revoked_at) VALUES (?, ?, 0, ?)")
      .run(createHash("sha256").update(token).digest("hex"), label, revokedAt);
  const post = async (lines: string[], token: string | null = "s3cret", method = "POST") => {
    const response = await worker.fetch(
      new Request("https://ingest.example/v1/events", {
        method,
        headers: token === null ? {} : { Authorization: `Bearer ${token}` },
        body: method === "POST" ? lines.join("\n") : undefined,
      }) as Parameters<typeof worker.fetch>[0],
      env,
    );
    return { status: response.status, body: (await response.json()) as Record<string, unknown> };
  };
  return { db, env, allow, post };
}

const event = (fields: Record<string, unknown> = {}) =>
  JSON.stringify({ event: "recall", at: 1_794_924_599_413, install: INSTALL, ...fields });

test("events are kept once, under the label of the token that sent them", async () => {
  const { db, allow, post } = endpoint();
  allow("s3cret", "nova");
  const lines = [
    event({ host: "mcp", version: "0.1.3", ms: 19, found: 0, tics: { negation: 2 } }),
    event({ event: "memory_pass", pass: "reflect", error: "llm", refused: ["remember/duplicate"] }),
  ];
  expect(await post(lines)).toEqual({ status: 200, body: { stored: 2, duplicates: 0, refused: 0 } });
  // The answer was lost and the client sent the lines again, with one more.
  expect(await post([...lines, event({ event: "open" })])).toEqual({
    status: 200,
    body: { stored: 1, duplicates: 2, refused: 0 },
  });

  const rows = db
    .query("SELECT sender, install, event, host, pass, error, ms, body FROM events ORDER BY id")
    .all() as Record<string, unknown>[];
  expect(rows.map((row) => row.event)).toEqual(["recall", "memory_pass", "open"]);
  expect(rows[0]).toMatchObject({ sender: "nova", install: INSTALL, host: "mcp", ms: 19 });
  expect(rows[1]).toMatchObject({ pass: "reflect", error: "llm", host: null, ms: null });
  // The fields without a column are in the body.
  expect(JSON.parse(rows[0]?.body as string).tics).toEqual({ negation: 2 });
});

test("a line that could hold something said is dropped, and the rest are kept", async () => {
  const { db, allow, post } = endpoint();
  allow("s3cret", "nova");
  const answer = await post([
    event(),
    "not json",
    // Longer than a word, a character no word has, a field no event has.
    event({ error: "x".repeat(65) }),
    event({ error: "My sister Marisol moved to Reykjavík" }),
    event({ "what she said": 1 }),
    // Not from an install, not at a time, nested deeper than an event is.
    event({ install: "nova's laptop" }),
    event({ at: "yesterday" }),
    event({ a: { b: { c: { d: 1 } } } }),
  ]);
  expect(answer).toEqual({ status: 200, body: { stored: 1, duplicates: 0, refused: 7 } });
  expect(db.query("SELECT count(*) AS n FROM events").get()).toEqual({ n: 1 });
});

test("only a token that was let in, and not revoked, stores anything", async () => {
  const { db, allow, post } = endpoint();
  allow("s3cret", "nova");
  allow("0ld", "gone", 1);
  for (const token of [null, "wr0ng", "0ld"]) {
    expect((await post([event()], token)).status).toBe(401);
  }
  expect(db.query("SELECT count(*) AS n FROM events").get()).toEqual({ n: 0 });
});

test("a request over its limits is refused whole, and so is anything but a POST", async () => {
  const { db, allow, post } = endpoint();
  allow("s3cret", "nova");
  const many = Array.from({ length: 501 }, (_, ms) => event({ ms }));
  expect((await post(many)).status).toBe(413);
  // Under 500 lines, over 512 KiB.
  const padding = "x".repeat(64);
  const fields = Object.fromEntries(Array.from({ length: 30 }, (_, i) => [`f_${"a".repeat(i)}`, padding]));
  const large = Array.from({ length: 300 }, (_, ms) => event({ ms, ...fields }));
  expect(large.join("\n").length).toBeGreaterThan(512 * 1024);
  expect((await post(large)).status).toBe(413);
  expect((await post([], "s3cret", "GET")).status).toBe(405);
  expect(db.query("SELECT count(*) AS n FROM events").get()).toEqual({ n: 0 });
});

test("a database that fails answers 503, so the sender keeps its events", async () => {
  const { db, allow, post } = endpoint();
  allow("s3cret", "nova");
  db.exec("DROP TABLE events");
  expect((await post([event()])).status).toBe(503);
});

test("the nightly pass deletes what came more than 90 days ago", async () => {
  const { db, env, allow, post } = endpoint();
  allow("s3cret", "nova");
  await post([event({ ms: 1 }), event({ ms: 2 }), event({ ms: 3 })]);
  // When it came decides, not when the sender says it happened.
  db.query("UPDATE events SET received_at = ? WHERE ms = 1").run(Date.now() - 91 * DAY_MS);
  db.query("UPDATE events SET received_at = ?, at = 1 WHERE ms = 2").run(Date.now() - 89 * DAY_MS);
  await worker.scheduled({} as ScheduledController, env);
  expect(db.query("SELECT ms FROM events ORDER BY ms").all()).toEqual([{ ms: 2 }, { ms: 3 }]);
});
